use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, SupportedOutputConfigs};
use futures::future::{join_all, poll_fn};
use futures::{Stream, StreamExt};
use livekit::options::{audio, TrackPublishOptions};
use livekit::participant::ParticipantKind;
use livekit::prelude::LocalParticipant;
use livekit::track::{LocalAudioTrack, LocalTrack, RemoteAudioTrack, RemoteTrack, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::native::audio_resampler::AudioResampler;
use livekit::webrtc::prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource};
use livekit::webrtc::stats::dictionaries::AudioPlayoutStats;
use livekit::{Room, RoomEvent, RoomOptions};
use log::{error, info, warn};
use pool::Pool;
use reqwest::{Client, StatusCode};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use std::vec;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};
use tonic::{Request, Response, Status};

const SAMPLE_RATE: u32 = 48000;
const SAMPLES_PER_10_MS: u32 = SAMPLE_RATE / 100; // 480 samples per 10ms
const SAMPLES_PER_100_MS: u32 = SAMPLES_PER_10_MS * 10; // 4800 samples per 100ms
const NUM_CHANNELS: u32 = 2;

pub mod pb {
    // tonic::include!("/pb/voip.rs"); // Generated from the proto file
    include!("pb/voip.rs");
}

use pb::voip_server_event::{EventPayload, EventType};
use pb::voip_service_server::VoipService;
use pb::{ClientAppConnected, ClientInfo, SendDebugEventPayload, VoipServerEvent};

use crate::config::Config;
use crate::model;

// Type alias for the streaming server response.
type ServerEventStream = Pin<Box<dyn Stream<Item = Result<VoipServerEvent, Status>> + Send>>;

type EventSenderChannel = mpsc::Sender<Result<VoipServerEvent, Status>>;

type ClientAppID = String;
type RemoteUserName = String;

// Main service struct.
pub struct MyVoipService {
    // Handle to a config object.
    config: Config,
    current_user_name: Option<String>,
    current_room_name: Option<String>,
    call_state: CallState,
    all_member_names: Vec<String>,
    http_client: Client,
    livekit_token: Option<String>,
    current_room: Option<Arc<Room>>,
    connected_client_ids: Vec<String>,
    pub event_sender_of_client: HashMap<ClientAppID, EventSenderChannel>,

    // Used to communicate with the event sender task.
    grpc_event_tx: Option<mpsc::UnboundedSender<VoipServerEvent>>,

    // Used to stop the audio thread of the corresponding participant.
    stop_audio_thread_tx_of_user: HashMap<RemoteUserName, tokio::sync::oneshot::Sender<bool>>,

    audio_frame_sender: tokio::sync::mpsc::Sender<FrameArray>,
    audio_frame_receiver: Option<tokio::sync::mpsc::Receiver<FrameArray>>, // Moved after we start the audio thread.

    frame_combiner: Option<SharedFrameCombiner>,
}

pub type SharedVoipService = Arc<Mutex<MyVoipService>>;

#[derive(Debug, Clone)]
pub enum CallState {
    Disconnected,
    Connecting,
    Connected,
}

impl Default for MyVoipService {
    fn default() -> Self {
        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(100);

        MyVoipService {
            config: Config::from_env_variables(),
            current_user_name: None,
            current_room_name: None,
            call_state: CallState::Disconnected,
            all_member_names: vec![],
            http_client: Client::new(),
            livekit_token: None,
            current_room: None,
            connected_client_ids: vec![],
            event_sender_of_client: HashMap::new(),
            grpc_event_tx: None,
            stop_audio_thread_tx_of_user: HashMap::new(),
            frame_combiner: None,
            audio_frame_sender: frame_tx,
            audio_frame_receiver: Some(frame_rx),
        }
    }
}

#[tonic::async_trait]
impl VoipService for SharedVoipService {
    type JoinRoomStream = ServerEventStream;

    async fn join_room(
        &self,
        request: Request<ClientInfo>,
    ) -> Result<Response<Self::JoinRoomStream>, Status> {
        let client_info = request.into_inner();
        println!(
            "Client {} registered for room {} with user_name {}",
            client_info.client_id, client_info.room_name, client_info.user_name
        );

        // Create a channel to send events to the client.
        let (tx, rx) = mpsc::channel(10);

        {
            // Lock the service to store the sender
            let mut service = self.lock().await;
            service
                .event_sender_of_client
                .insert(client_info.client_id.clone(), tx.clone());
        }

        // Convert the receiver into a stream.
        let stream = ReceiverStream::new(rx);

        // Connect to LiveKit
        let res = connect_to_livekit(
            self.clone(),
            &client_info.client_id,
            &client_info.user_name,
            &client_info.room_name,
        )
        .await;

        let event_stream_result: Result<Response<Self::JoinRoomStream>, Status> = match res {
            Ok(_) => {
                info!("Connected to LiveKit room {}", client_info.room_name);
                Ok(Response::new(Box::pin(stream) as Self::JoinRoomStream))
            }
            Err(err) => {
                error!("Failed to connect to LiveKit room: {}", err);
                Err(Status::internal("Failed to connect to LiveKit"))
            }
        };

        info!("Returning from join_room");

        // Send a client_app_connected event back to the client after 2 seconds.
        let self_ref = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let service = self_ref.lock().await;
            if let Some(sender) = service.event_sender_of_client.get(&client_info.client_id) {
                // Get room info and send event
                let room_info = service.current_room_info().await.unwrap();
                let active_speaker_ids = room_info
                    .remote_participants
                    .iter()
                    .filter(|participant| participant.is_speaking)
                    .map(|participant| participant.identity.clone())
                    .collect();

                let existing_member_ids = room_info
                    .remote_participants
                    .iter()
                    .map(|participant| participant.identity.clone())
                    .collect();

                let event = VoipServerEvent {
                    event_type: EventType::ClientAppConnected as i32,
                    current_user_name: client_info.user_name.clone(),
                    current_room_name: client_info.room_name.clone(),
                    event_payload: Some(EventPayload::ClientAppConnected(ClientAppConnected {
                        existing_member_ids,
                        existing_active_speaker_ids: active_speaker_ids,
                    })),
                };

                if sender.send(Ok(event)).await.is_err() {
                    warn!("Failed to send event to client");
                } else {
                    info!("Sent ping event to client");
                }
            }
        });

        event_stream_result
    }

    async fn send_debug_event_to_client(
        &self,
        request: Request<SendDebugEventPayload>,
    ) -> Result<Response<VoipServerEvent>, Status> {
        let payload = request.into_inner();

        let service = self.lock().await;

        let voip_service_event = payload.event.unwrap();

        let sender = service
            .event_sender_of_client
            .get(&payload.dest_client_id)
            .unwrap(); // TODO: Remove unwrap from here and handle the error.

        info!("Sending debug event to client {}", payload.dest_client_id);

        // Send the event using the mpsc sender if available
        let send_res = sender.send(Ok(voip_service_event.clone())).await;

        match send_res {
            Ok(_) => Ok(Response::new(voip_service_event)),
            Err(e) => {
                error!("Failed to send event to client {}", e);
                Err(Status::internal("Failed to send event to client"))
            }
        }
    }
}

pub async fn connect_to_livekit(
    service: SharedVoipService,
    client_id: &str,
    user_name: &str,
    room_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut service_guard = service.lock().await;

    info!(
        "Fetching join token for self {} in room {}",
        user_name, room_name
    );

    let url = reqwest::Url::parse_with_params(
        &service_guard.config.livekit_token_endpoint,
        &[("room", room_name), ("identity", user_name)],
    )?;

    info!("Fetching join token from {}", url);

    let resp = service_guard.http_client.get(url).send().await?;
    if !matches!(resp.status(), StatusCode::OK) {
        error!(
            "Failed to get livekit token for room {}. HTTP Error: {}",
            room_name,
            resp.status()
        );
        return Err("Failed to get livekit token".into());
    }

    let token = resp.text().await?;

    info!("Received token {}", token);

    service_guard.call_state = CallState::Connecting;

    let options = RoomOptions::default();
    let connect_result =
        Room::connect(&service_guard.config.livekit_endpoint, &token, options).await;

    match connect_result {
        Ok((room, room_events)) => {
            info!("Connected to LiveKit room {}", room_name);

            // Save the room handle and info.
            service_guard.current_user_name = Some(user_name.to_string());
            service_guard.current_room_name = Some(room_name.to_string());
            service_guard.current_room = Some(Arc::new(room));
            service_guard.call_state = CallState::Connected;
            service_guard.all_member_names = vec![user_name.to_string()];
            service_guard.livekit_token = Some(token);

            info!("connect_to_livekit call returned");

            // Start a separate task to handle room events.
            tokio::spawn(handle_room_events(
                service.clone(),
                user_name.to_string(),
                room_name.to_string(),
                room_events,
            ));

            // Start a task to send grpc events to the client.
            let (grpc_event_tx, grpc_event_rx) = mpsc::unbounded_channel();
            service_guard.grpc_event_tx = Some(grpc_event_tx);
            tokio::spawn(send_event_to_clients(service.clone(), grpc_event_rx));

            // Start a task to publish the local audio track.
            // Call the function and handle the error if any
            if service_guard.config.disable_local_audio_capture {
                info!("Local audio capture DISABLED, not starting audio capture");
            } else {
                tokio::spawn(start_capturing_audio_input(service.clone()));
                info!("Done starting audio capture");
            }

            Ok(())
        }

        Err(err) => {
            error!("Failed to connect to LiveKit room: {}", err);
            Err("Failed to connect to LiveKit".into())
        }
    }
}

pub async fn start_audio_playback(service: SharedVoipService) -> JoinHandle<()> {
    let frame_combiner = SharedFrameCombiner::new();

    let mut audio_frame_sender = None;

    {
        let mut guard = service.lock().await;

        let frame_receiver = guard.audio_frame_receiver.take().unwrap();
        spawn_cpal_playback_thread(frame_receiver);

        audio_frame_sender = Some(guard.audio_frame_sender.clone());

        guard.frame_combiner = Some(SharedFrameCombiner(frame_combiner.0.clone()));

        info!("Started cpal playback thread");
    }

    // Start the frame combiner task.
    tokio::task::spawn_local(async move {
        info!("Starting frame combiner polling task in local set");
        frame_combiner
            .keep_polling(audio_frame_sender.unwrap())
            .await
    })
}

impl MyVoipService {
    // pub async fn start_audio_playback(&mut self) {
    //     // Start the audio playback thread.
    //     let frame_receiver = self.audio_frame_receiver.take().unwrap();
    //     spawn_cpal_playback_thread(frame_receiver);

    //     info!("Started cpal playback thread");

    //     let frame_combiner = SharedFrameCombiner::new();

    //     self.frame_combiner = Some(SharedFrameCombiner(frame_combiner.0.clone()));

    //     let audio_frame_sender = self.audio_frame_sender.clone();

    //     // Start the frame combiner task.
    //     let result = tokio::task::spawn_local(async move {
    //         info!("Starting frame combiner polling task in local set");
    //         frame_combiner.keep_polling(audio_frame_sender).await
    //     })
    //     .await;
    //     // info!("Started frame combiner polling task");
    // }

    // Method to print room info (list of participants)
    pub async fn current_room_info(&self) -> Result<model::RoomInfo, &'static str> {
        if self.current_room.is_none() {
            return Err("Not connected to a room").into();
        }

        let room = self.current_room.as_ref().unwrap();

        let mut room_info = model::RoomInfo {
            local_participant: model::ParticipantInfo {
                identity: self.current_user_name.as_ref().unwrap().to_string(),
                is_connected: true,
                is_speaking: false,
                audio_level: 0.0,
                kind: ParticipantKind::Standard,
            },
            remote_participants: vec![],
        };

        let local = room.local_participant();
        room_info.local_participant.is_speaking = local.is_speaking();
        room_info.local_participant.audio_level = local.audio_level();

        for (identity, participant) in room.remote_participants() {
            room_info.remote_participants.push(model::ParticipantInfo {
                identity: identity.as_str().to_string(),
                is_connected: true,
                is_speaking: participant.is_speaking(),
                audio_level: participant.audio_level(),
                kind: participant.kind(),
            });
        }

        return Ok(room_info);
    }
}

async fn handle_room_events(
    service: SharedVoipService,
    current_user_name: String,
    current_room_name: String,
    mut room_events: UnboundedReceiver<RoomEvent>,
) {
    while let Some(event) = room_events.recv().await {
        match event {
            RoomEvent::ParticipantConnected(participant) => {
                println!("Participant connected: {}", participant.identity());

                let mut service_guard = service.lock().await;

                println!("Acquired lock before sending member_joined event");

                service_guard
                    .all_member_names
                    .push(participant.identity().to_string());

                // Send event_type_member_joined to all connected clients.
                let event = VoipServerEvent {
                    event_type: EventType::MemberJoined as i32,
                    current_user_name: current_user_name.to_string(),
                    current_room_name: current_room_name.to_string(),
                    event_payload: Some(EventPayload::MemberJoined(pb::MemberJoined {
                        member_id: participant.identity().to_string(),
                    })),
                };

                if let Some(grpc_event_tx) = &service_guard.grpc_event_tx {
                    if let Err(e) = grpc_event_tx.send(event) {
                        error!("Failed to send event to client: {}", e);
                    }
                }

                info!("Sent member_joined event to all clients");
            }

            RoomEvent::ParticipantDisconnected(participant) => {
                println!("Participant disconnected: {}", participant.identity());

                let mut service_guard = service.lock().await;

                // Remove the participant from the list of all members.
                service_guard
                    .all_member_names
                    .retain(|name| name != participant.identity().as_str());

                // Send event_type_member_left to all connected clients.
                let event = VoipServerEvent {
                    event_type: EventType::MemberLeft as i32,
                    current_user_name: current_user_name.to_string(),
                    current_room_name: current_room_name.to_string(),
                    event_payload: Some(EventPayload::MemberLeft(pb::MemberLeft {
                        member_id: participant.identity().to_string(),
                    })),
                };

                if let Some(grpc_event_tx) = &service_guard.grpc_event_tx {
                    if let Err(e) = grpc_event_tx.send(event) {
                        error!("Failed to send event to client: {}", e);
                    }
                }
            }

            RoomEvent::ActiveSpeakersChanged { speakers } => {
                println!("Active speakers changed: {:?}", speakers);

                // Send event_type_active_speakers_changed to all connected clients.

                let service_guard = service.lock().await;

                let event = VoipServerEvent {
                    event_type: EventType::ActiveSpeakersChanged as i32,
                    current_user_name: current_user_name.to_string(),
                    current_room_name: current_room_name.to_string(),
                    event_payload: Some(EventPayload::ActiveSpeakersChanged(
                        pb::ActiveSpeakersChanged {
                            active_speaker_ids: speakers
                                .iter()
                                .map(|participant| participant.identity().to_string())
                                .collect(),
                        },
                    )),
                };

                if let Some(grpc_event_tx) = &service_guard.grpc_event_tx {
                    if let Err(e) = grpc_event_tx.send(event) {
                        error!("Failed to send event to client: {}", e);
                    }
                }
            }

            RoomEvent::LocalTrackPublished {
                publication,
                track: _,
                participant: _,
            } => {
                println!("Local track published: {:?}", publication);
            }

            RoomEvent::TrackSubscribed {
                track,
                publication: _,
                participant,
            } => {
                let remote_user_name = participant.identity().as_str().to_string();

                info!(
                    "Track subscribed from partipant. user_name: {:?}, track_sid: {:?}",
                    participant.identity(),
                    track.sid().as_str()
                );

                if let RemoteTrack::Audio(track) = track {
                    info!(
                        "Acquiring lock: Received audio track from remote user: {}",
                        remote_user_name
                    );
                    // > New audio thread
                    let mut service_guard = service.lock().await;

                    info!("Acquired lock for handling audio frame");

                    // Check if there's an existing stop signal for the remote user.
                    if let Some(stop_audio_thread_tx) = service_guard
                        .stop_audio_thread_tx_of_user
                        .remove(&remote_user_name)
                    // Remove before sending the signal
                    {
                        // Send a stop signal to the audio thread.
                        let _ = stop_audio_thread_tx.send(true); // Ignore the result, as the thread might already be stopped.
                    }

                    // Create a new stop signal for the audio thread.
                    // TODO: Simply remove the audio stream from the frame combiner. We don't need to use channels for it.
                    let (stop_audio_thread_tx, stop_audio_thread_rx) =
                        tokio::sync::oneshot::channel();

                    // Store the new stop signal in the service.
                    service_guard
                        .stop_audio_thread_tx_of_user
                        .insert(remote_user_name.clone(), stop_audio_thread_tx);

                    if !service_guard.config.disable_remote_audio_playback {
                        // play_audio_stream(track, stop_audio_thread_rx).await
                        handle_audio_frame(&mut service_guard, track)
                            .await
                            .expect("Failed to handle audio frame");
                    } else {
                        info!("Remote audio playback DISABLED");
                    }

                    // < New audio thread
                }
            }

            RoomEvent::TrackUnsubscribed {
                track,
                publication,
                participant,
            } => {}

            RoomEvent::LocalTrackSubscribed { track } => {
                println!("Local track subscribed: {:?}", track);
            }

            RoomEvent::TrackSubscriptionFailed {
                participant,
                error,
                track_sid,
            } => {
                println!(
                    "Track subscription failed: {:?}, {:?}, {:?}",
                    participant, error, track_sid
                );
            }

            RoomEvent::ConnectionQualityChanged {
                quality: _,
                participant: _,
            } => {
                // Do nothing
            }

            _ => {
                println!("Unhandled room event: {:?}", event);
            }
        }
    }
}

// Keeps sending events to all connected clients.
async fn send_event_to_clients(
    service: SharedVoipService,
    mut event_receiver: UnboundedReceiver<VoipServerEvent>,
) {
    while let Some(event) = event_receiver.recv().await {
        let service_guard = service.lock().await;

        for (client_id, sender) in service_guard.event_sender_of_client.iter() {
            if let Err(e) = sender.send(Ok(event.clone())).await {
                error!("Failed to send event to client {}: {}", client_id, e);
            }
        }
    }
}

fn spawn_cpal_playback_thread(mut frame_receiver: tokio::sync::mpsc::Receiver<FrameArray>) {
    // Get the default audio output device
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("Failed to find default output device");

    let mut must_have_config = None;

    // Seems like the buffer size is provided as bytes in SupportedStreamConfig while in StreamConfig it should be provided as samples.
    let wanted_buffer_size = (SAMPLES_PER_10_MS * NUM_CHANNELS) * 4; // 4 bytes per f32 sample

    // Print all output configs supported. Choose one with our SAMPLE_RATE and NUM_CHANNELS=2 and buffer size of 10ms = 480 samples => 480 * 2 = 960 f32 scalars.
    let output_configs = device.supported_output_configs().unwrap();
    for config in output_configs {
        info!("Supported output config: {:?}", config);

        if must_have_config.is_some() {
            continue;
        }

        let sr_min = config.min_sample_rate();
        let sr_max = config.max_sample_rate();

        let sr_buffer_size = match config.buffer_size().clone() {
            cpal::SupportedBufferSize::Range { min, max } => {
                if min <= wanted_buffer_size && wanted_buffer_size <= max {
                    Some(wanted_buffer_size)
                } else {
                    None
                }
            }

            cpal::SupportedBufferSize::Unknown => None,
        };

        // Usually we will have 48k sample rate and 2 channels. Hardcoding it for that.
        if sr_min == SampleRate(SAMPLE_RATE)
            && sr_max == SampleRate(SAMPLE_RATE)
            && sr_buffer_size.is_some()
        {
            must_have_config = Some(cpal::StreamConfig {
                channels: NUM_CHANNELS as u16,
                sample_rate: SampleRate(SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Fixed(SAMPLES_PER_10_MS),
            });
        }
    }

    // Set up the output format
    // let mut config: cpal::StreamConfig = device
    //     .default_output_config()
    //     .expect("Failed to get default device output config")
    //     .into();

    let mut config = must_have_config.expect("Failed to find suitable output config");

    info!("Using output config: {:?}", config);

    // Start a separate thread to handle the audio playback using cpal
    std::thread::spawn(move || {
        let mut frame_receiver = frame_receiver;
        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| loop {
                    if let Ok(frame_array) = frame_receiver.try_recv() {
                        // info!("cpal device buffer length = {}", data.len());

                        for (i, sample) in frame_array.data.iter().enumerate() {
                            if i < data.len() {
                                data[i] = *sample;
                            }
                        }
                    } else {
                        break;
                    }
                },
                move |err| {
                    eprintln!("Error occurred on stream: {}", err);
                },
                None, // No timeout needed here
            )
            .unwrap();

        // Start the stream in the separate thread
        if let Err(err) = stream.play() {
            error!(
                "Failed to play audio stream on device: {}, error: {}",
                device.name().unwrap(),
                err
            );
        } else {
            info!("Playing audio stream on device: {}", device.name().unwrap());
        }

        // Keep the thread alive for the duration of the stream
        std::thread::park();
    });
}

// TODO: Take a command channel in order to exit the audio playback loop.
// TODO: Remove this. Not using it anymore.
async fn play_audio_stream(
    audio_track: RemoteAudioTrack,
    stop_audio_thread_rx: tokio::sync::oneshot::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Start audio stream at time {:?}", std::time::Instant::now());

    // Get the default audio output device
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .expect("Failed to find default output device");

    // Set up the output format
    let config = device.default_output_config()?.into();

    let sample_rate = SAMPLE_RATE;
    // let bit_depth = 16;
    let num_channels = 2;

    // Set up the resampler
    let mut resampler = AudioResampler::default();
    let rtc_track = audio_track.rtc_track();
    let mut audio_stream =
        NativeAudioStream::new(rtc_track, sample_rate as i32, num_channels as i32);

    // Build a channel to send audio samples
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Vec<f32>>(10);

    // Start a separate thread to handle the audio playback using cpal
    std::thread::spawn(move || {
        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    loop {
                        if let Ok(samples) = receiver.try_recv() {
                            // info!("Received {} samples from cpal thread", samples.len());

                            for (i, sample) in samples.iter().enumerate() {
                                if i < data.len() {
                                    data[i] = *sample;
                                }
                            }
                        } else {
                            // If no new samples, fill with silence.
                            // info!("No new samples, filling with silence");
                            // for sample in data.iter_mut() {
                            //     *sample = 0.0;
                            // }
                            break;
                        }
                    }
                },
                move |err| {
                    eprintln!("Error occurred on stream: {}", err);
                },
                None, // No timeout needed here
            )
            .unwrap();

        // Start the stream in the separate thread
        if let Err(err) = stream.play() {
            error!(
                "Failed to play audio stream on device: {}, error: {}",
                device.name().unwrap(),
                err
            );
        } else {
            info!("Playing audio stream on device: {}", device.name().unwrap());
        }

        // Keep the thread alive for the duration of the stream
        std::thread::park(); // Thread will sleep here until manually woken
    });

    // Receive new samples from the audio track.
    tokio::spawn(async move {
        let start_time = std::time::Instant::now();
        let mut last_dt = std::time::Instant::now();

        let mut printed_audio_frame_info = false;

        loop {
            match audio_stream.next().await {
                Some(frame) => {
                    if !printed_audio_frame_info {
                        let new_dt = std::time::Instant::now();
                        let dt = new_dt - last_dt;
                        last_dt = new_dt;

                        log::info!(
                            "Receiving audio frame with {} samples - {:?}",
                            frame.data.len(),
                            dt.as_nanos()
                        );

                        printed_audio_frame_info = true;
                    }

                    // Wait for the consumed samples buffer to have space
                    // let data_f32 = consumed_samples_buffer_rx.recv().await.unwrap();

                    // Resample the audio frame
                    let data_i16 = resampler.remix_and_resample(
                        &frame.data,
                        frame.samples_per_channel,
                        frame.num_channels,
                        frame.sample_rate,
                        num_channels,
                        sample_rate,
                    );

                    // Convert i16 samples to f32 before sending to cpal
                    let data_f32: Vec<f32> = data_i16
                        .iter()
                        .map(|&sample| sample as f32 / i16::MAX as f32) // Normalize i16 to f32
                        .collect();

                    let _ = sender.send(data_f32).await; // Send the f32 samples to the cpal callback
                                                         // fresh_samples_buffer_tx.send(data_f32).await.unwrap();
                }
                None => {
                    info!("No more audio frames to play, closing cpal stream");
                    break;
                }
            }
        }
    });

    Ok(())
}

async fn handle_audio_frame(
    voip_service: &mut MyVoipService,
    audio_track: RemoteAudioTrack,
) -> Result<(), Box<dyn std::error::Error>> {
    // Add it to the frame combiner
    if voip_service.config.disable_remote_audio_playback {
        info!("Remote audio playback DISABLED, not adding audio stream to frame combiner");
        return Ok(());
    }

    let audio_stream = NativeAudioStream::new(
        audio_track.rtc_track(),
        SAMPLE_RATE as i32,
        NUM_CHANNELS as i32,
    );

    if let Some(frame_combiner) = &voip_service.frame_combiner {
        frame_combiner.add_audio_stream(audio_stream).await;
    }

    Ok(())
}

async fn publish_local_track(
    local_participant: LocalParticipant,
    sample_rate: u32,
    num_channels: u32,
) -> Result<NativeAudioSource, Box<dyn std::error::Error + Sync + Send>> {
    // Create a NativeAudioSource
    let native_audio_source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: true,
            noise_suppression: true,
            auto_gain_control: true,
        },
        sample_rate,
        num_channels,
        None,
    );

    let audio_source = RtcAudioSource::Native(native_audio_source.clone());

    let local_audio_track = LocalAudioTrack::create_audio_track("local_track", audio_source);

    let mut track_publish_options = TrackPublishOptions::default();
    track_publish_options.source = TrackSource::Microphone;

    // Publish the track to LiveKit
    local_participant
        .publish_track(LocalTrack::Audio(local_audio_track), track_publish_options)
        .await?;

    info!("Local audio track published");

    Ok(native_audio_source)
}

async fn start_capturing_audio_input(
    voip_service: SharedVoipService,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("Failed to get default input device");
    let mut config: cpal::StreamConfig = device.default_input_config()?.into();

    config.channels = NUM_CHANNELS as u16;
    config.sample_rate = SampleRate(SAMPLE_RATE);

    // Create a channel for sending frames from the callback to an async task
    let (tx, mut rx) = mpsc::channel::<AudioFrame>(10);

    // Lock the service to access the room and publish the track
    let native_audio_source = {
        let service_guard = voip_service.lock().await;

        if let Some(room) = &service_guard.current_room {
            let native_audio_source =
                publish_local_track(room.local_participant(), SAMPLE_RATE, NUM_CHANNELS).await?;

            native_audio_source
        } else {
            return Err("No active room found".into());
        }
    };

    // Spawn a separate thread to handle the audio stream
    std::thread::spawn(move || {
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _| {
                    // info!("Received audio samples from cpal: {}", data.len());

                    // Convert PCM samples to i16, assuming they are normalized [-1.0, 1.0] floats
                    let pcm_samples: Vec<i16> = data
                        .iter()
                        .map(|&sample| (sample * i16::MAX as f32) as i16)
                        .collect();

                    let frame_size = pcm_samples.len();

                    // Create an audio frame for NativeAudioSource
                    let audio_frame = AudioFrame {
                        data: pcm_samples.into(),
                        num_channels: NUM_CHANNELS,
                        sample_rate: SAMPLE_RATE,
                        samples_per_channel: (frame_size / NUM_CHANNELS as usize) as u32,
                    };

                    // Send the audio frame to the async task via the channel
                    if let Err(e) = tx.try_send(audio_frame) {
                        eprintln!("Failed to send audio frame: {}", e);
                    }
                },
                move |err| {
                    eprintln!("Stream error: {}", err);
                },
                None,
            )
            .expect("Failed to build input stream");

        // Start the stream
        stream.play().expect("Failed to play the audio stream");

        // Park the thread to keep it alive for audio capture
        std::thread::park();
    });

    // Spawn a separate async task to handle audio frames
    tokio::spawn(async move {
        while let Some(audio_frame) = rx.recv().await {
            // Capture the frame asynchronously using NativeAudioSource
            // info!("Input frame count: {}", audio_frame.data.len());
            if let Err(e) = native_audio_source.capture_frame(&audio_frame).await {
                eprintln!("Failed to capture frame: {}", e);
            }
        }
    });

    Ok(())
}

const AUDIO_FRAME_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
struct FrameArray {
    data: [f32; (SAMPLES_PER_10_MS * NUM_CHANNELS) as usize],
}

impl FrameArray {
    fn new() -> Self {
        FrameArray {
            data: [0.0; (SAMPLES_PER_10_MS * NUM_CHANNELS) as usize],
        }
    }
}

impl Default for FrameArray {
    fn default() -> Self {
        FrameArray {
            data: [0.0; (SAMPLES_PER_10_MS * NUM_CHANNELS) as usize],
        }
    }
}

impl Clone for FrameArray {
    fn clone(&self) -> Self {
        FrameArray { data: self.data }
    }
}

// A struct to combine audio frames from multiple audio streams.
#[derive(Debug)]
struct FrameCombiner {
    // TODO: Instead of audio_streams vec, use a hashmap with the track sid as
    // the key and allow removal of streams when track is unsubscribed.
    audio_streams: Vec<NativeAudioStream>,
}

struct SharedFrameCombiner(Arc<std::sync::Mutex<FrameCombiner>>);

impl SharedFrameCombiner {
    fn new() -> Self {
        let fc = FrameCombiner {
            audio_streams: vec![],
        };

        SharedFrameCombiner(Arc::new(std::sync::Mutex::new(fc)))
    }

    async fn add_audio_stream(&self, audio_stream: NativeAudioStream) {
        info!("Adding audio stream to frame combiner");
        let mut fc_mutex_guard = self.0.lock().expect("Failed to lock frame combiner");
        fc_mutex_guard.audio_streams.push(audio_stream);

        info!("Done adding audio stream to frame combiner");
    }

    async fn keep_polling(&self, frame_sender: tokio::sync::mpsc::Sender<FrameArray>) {
        let interval = tokio::time::interval(AUDIO_FRAME_POLL_INTERVAL);
        tokio::pin!(interval);

        info!("Starting audio frame polling loop");

        loop {
            interval.as_mut().tick().await;

            // info!("Polling for audio frames");

            // Lock the state to access the audio streams
            let mut self_guard = self.0.lock().expect("Failed to lock frame combiner");

            // info!("Acquired lock for audio streams");

            // Allocate a new mix buffer for mixing audio frames
            let mut mix_buffer = FrameArray::new();

            // Iterate over audio streams and poll for frames
            for audio_stream in self_guard.audio_streams.iter_mut() {
                // Poll for the next frame without blocking
                let mut frame_future = audio_stream.next();

                // Poll the future directly
                let frame = poll_fn(|cx| Pin::new(&mut frame_future).poll(cx)).await;

                // Process the frame if it exists
                if let Some(frame) = frame {
                    // Ensure the frame has the correct number of samples
                    if frame.data.len() != (SAMPLES_PER_10_MS * 2) as usize {
                        error!(
                            "Received frame with {} samples, expected {}",
                            frame.data.len(),
                            SAMPLES_PER_10_MS * 2
                        );
                        continue;
                    }

                    // Add the frame to the mix buffer
                    for (i, sample) in frame.data.iter().enumerate() {
                        mix_buffer.data[i] += (*sample as f32) / i16::MAX as f32;
                    }
                } else {
                    // Handle the case where the stream has ended or timed out
                    warn!("Audio stream ended or timed out");
                }
            }

            // Send the mix buffer to the mixed frame producer
            if let Err(e) = frame_sender.send(mix_buffer).await {
                error!("Failed to send frame: {:?}", e);
            }
        }
    }
}
