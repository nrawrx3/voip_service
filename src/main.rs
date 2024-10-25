use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleRate;
use futures::{Stream, StreamExt};
use livekit::options::TrackPublishOptions;
use livekit::participant::ParticipantKind;
use livekit::prelude::LocalParticipant;
use livekit::track::{LocalAudioTrack, LocalTrack, RemoteAudioTrack, RemoteTrack, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::native::audio_resampler::AudioResampler;
use livekit::webrtc::prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource};
use livekit::{Room, RoomEvent, RoomOptions};
use log::{error, info, warn};
use reqwest::{Client, StatusCode};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{transport::Server, Request, Response, Status};

mod model;
mod wav;

mod perf_samples;

mod config;
use config::Config;

const SAMPLE_RATE: u32 = 48000;
const NUM_CHANNELS: u32 = 2;

pub mod pb {
    // tonic::include!("/pb/voip.rs"); // Generated from the proto file
    include!("pb/voip.rs");
}

use pb::voip_server_event::{EventPayload, EventType};
use pb::voip_service_server::{VoipService, VoipServiceServer};
use pb::{ClientInfo, SendDebugEventPayload, VoipServerEvent};

// Type alias for the streaming server response.
type ServerEventStream = Pin<Box<dyn Stream<Item = Result<VoipServerEvent, Status>> + Send>>;

type EventSenderChannel = mpsc::Sender<Result<VoipServerEvent, Status>>;

type ClientAppID = String;
type RemoteUserName = String;

// Main service struct.
pub struct MyVoipService {
    // Handle to a config object.
    config: Config,
    pub current_user_name: Option<String>,
    pub current_room_name: Option<String>,
    pub call_state: CallState,
    pub all_member_names: Vec<String>,
    pub http_client: Client,
    pub livekit_token: Option<String>,
    pub current_room: Option<Arc<Room>>,
    pub connected_client_ids: Vec<String>,
    pub event_sender_of_client: HashMap<ClientAppID, EventSenderChannel>,

    // Used to communicate with the event sender task.
    pub grpc_event_tx: Option<mpsc::UnboundedSender<VoipServerEvent>>,

    // Used to stop the audio thread of the corresponding participant.
    pub stop_audio_thread_tx_of_user: HashMap<RemoteUserName, tokio::sync::oneshot::Sender<bool>>,
}

type SharedVoipService = Arc<Mutex<MyVoipService>>;

#[derive(Debug, Clone)]
pub enum CallState {
    Disconnected,
    Connecting,
    Connected,
}

impl Default for MyVoipService {
    fn default() -> Self {
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

        let return_value: Result<Response<Self::JoinRoomStream>, Status> = match res {
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

        let self_ref = self.clone();

        // Send a ping event back to the client after 2 seconds.
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let service = self_ref.lock().await;
            let sender = service
                .event_sender_of_client
                .get(&client_info.client_id)
                .unwrap(); // TODO: Remove unwrap from here.

            let event = VoipServerEvent {
                event_type: EventType::Ping as i32,
                current_user_name: client_info.user_name.clone(),
                current_room_name: client_info.room_name.clone(),
                event_payload: None,
            };

            if sender.send(Ok(event)).await.is_err() {
                warn!("Failed to send event to client");
            } else {
                info!("Sent ping event to client");
            }
        });

        return_value
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

            let local_participant = room.local_participant();

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

impl MyVoipService {
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
                    // > New audio thread
                    let mut service_guard = service.lock().await;

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
                    let (stop_audio_thread_tx, stop_audio_thread_rx) =
                        tokio::sync::oneshot::channel();

                    // Store the new stop signal in the service.
                    service_guard
                        .stop_audio_thread_tx_of_user
                        .insert(remote_user_name.clone(), stop_audio_thread_tx);

                    if !service_guard.config.disable_remote_audio_playback {
                        tokio::spawn(async move {
                            play_audio_stream(track, stop_audio_thread_rx).await
                        });
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

// TODO: Take a command channel in order to exit the audio playback loop.
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

    let sample_rate = 48000;
    let bit_depth = 16;
    let num_channels = 2;

    // Set up the resampler
    let mut resampler = AudioResampler::default();
    let rtc_track = audio_track.rtc_track();
    let mut audio_stream =
        NativeAudioStream::new(rtc_track, sample_rate as i32, num_channels as i32);

    // Build a channel to send audio samples
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Vec<f32>>(10);

    // Double buffer for audio samples. Fresh samples = freshly received samples
    // from the audio track. Consumed samples = samples that have been sent to
    // the cpal stream.
    let (fresh_samples_buffer_tx, mut fresh_samples_buffer_rx) =
        tokio::sync::mpsc::channel::<Vec<f32>>(2);
    let (consumed_samples_buffer_tx, mut consumed_samples_buffer_rx) =
        tokio::sync::mpsc::channel::<Vec<f32>>(2);

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
        loop {
            match audio_stream.next().await {
                Some(frame) => {
                    let new_dt = std::time::Instant::now();
                    let dt = new_dt - last_dt;
                    last_dt = new_dt;

                    log::info!(
                        "Received audio frame with {} samples - {:?}",
                        frame.data.len(),
                        dt.as_nanos()
                    );

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
            if let Err(e) = native_audio_source.capture_frame(&audio_frame).await {
                eprintln!("Failed to capture frame: {}", e);
            }
        }
    });

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let voip_service = Arc::new(Mutex::new(MyVoipService::default()));

    // Create a channel to receive shutdown signal from the REPL and close the gRPC server.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let addr = "[::1]:50051".parse().unwrap();

    info!("Starting grpc server at {}", addr);

    Server::builder()
        .add_service(VoipServiceServer::new(voip_service))
        .serve_with_shutdown(addr, async {
            shutdown_rx.await.ok();
            info!("Shutting down grpc server");
        })
        .await?;

    Ok(())
}
