use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::{Stream, StreamExt};
use livekit::participant::ParticipantKind;
use livekit::track::{RemoteAudioTrack, RemoteTrack};
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::native::audio_resampler::AudioResampler;
use livekit::{Room, RoomEvent, RoomOptions};
use log::{error, info, warn};
use reqwest::{Client, StatusCode};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{transport::Server, Request, Response, Status};
use wav::WavHeader;

mod model;
mod wav;

mod config;
use config::Config;

pub mod pb {
    // tonic::include!("/pb/voip.rs"); // Generated from the proto file
    include!("pb/voip.rs");
}

use pb::voip_server_event::{EventPayload, EventType};
use pb::voip_service_server::{VoipService, VoipServiceServer};
use pb::{ClientInfo, SendDebugEventPayload, VoipServerEvent};

// Type alias for the streaming server response.
type ServerEventStream = Pin<Box<dyn Stream<Item = Result<VoipServerEvent, Status>> + Send>>;

// Main service struct.
pub struct MyVoipService {
    // Handle to a config object.
    config: Config,
    pub current_user_name: Option<String>,
    pub current_room_name: Option<String>,
    pub call_state: CallState,
    pub all_member_names: Vec<String>,
    pub client: Client,
    pub livekit_token: Option<String>,
    pub current_room: Option<Arc<Room>>,
    pub event_sender: Option<mpsc::Sender<Result<VoipServerEvent, Status>>>,
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
            client: Client::new(),
            livekit_token: None,
            current_room: None,
            event_sender: None,
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
            "Client {} registered for room {}",
            client_info.client_id, client_info.room_id
        );

        // Create a channel to send events to the client.
        let (tx, rx) = mpsc::channel(10);

        {
            // Lock the service to store the sender
            let mut service = self.lock().await;
            service.event_sender = Some(tx.clone()); // Store the sender in the struct
        }

        // Convert the receiver into a stream.
        let stream = ReceiverStream::new(rx);

        // Connect to LiveKit
        let res =
            locked_connect_to_livekit(self.clone(), &client_info.client_id, &client_info.room_id)
                .await;

        let return_value: Result<Response<Self::JoinRoomStream>, Status> = match res {
            Ok(_) => {
                info!("Connected to LiveKit room {}", client_info.room_id);
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
            if let Some(sender) = &self_ref.lock().await.event_sender {
                let event = VoipServerEvent {
                    event_type: EventType::Ping as i32,
                    current_user_id: client_info.client_id.clone(),
                    room_id: client_info.room_id.clone(),
                    event_payload: None,
                };

                if sender.send(Ok(event)).await.is_err() {
                    warn!("Failed to send event to client");
                } else {
                    info!("Sent ping event to client");
                }
            }
        });

        return_value
    }

    async fn send_debug_event_to_client(
        &self,
        request: Request<SendDebugEventPayload>,
    ) -> Result<Response<VoipServerEvent>, Status> {
        let payload = request.into_inner();

        // Create the event.
        let voip_service_event = VoipServerEvent {
            event_type: payload.event_type as i32,
            current_user_id: self
                .lock()
                .await
                .current_user_name
                .clone()
                .unwrap_or("none".to_string()),
            room_id: self
                .lock()
                .await
                .current_room_name
                .clone()
                .unwrap_or("none".to_string()),
            event_payload: Some(EventPayload::MemberJoined(pb::MemberJoined {
                member_id: "dummy".to_string(),
            })),
        };

        // Send the event using the mpsc sender if available
        if let Some(sender) = &self.lock().await.event_sender {
            if sender.send(Ok(voip_service_event.clone())).await.is_err() {
                warn!("Failed to send event to client");
            }
        }

        Ok(Response::new(voip_service_event))
    }
}

pub async fn locked_connect_to_livekit(
    service: SharedVoipService,
    user_name: &str,
    room_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut service = service.lock().await;

    let result = service.connect_to_livekit(&user_name, &room_name).await;

    info!("connect_to_livekit call returned");

    result
}

impl MyVoipService {
    pub async fn connect_to_livekit(
        &mut self,
        user_name: &str,
        room_name: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!(
            "Fetching join token for self {} in room {}",
            user_name, room_name
        );

        let url = reqwest::Url::parse_with_params(
            &self.config.livekit_token_endpoint,
            &[("room", room_name), ("identity", user_name)],
        )?;

        info!("Fetching join token from {}", url);

        let resp = self.client.get(url).send().await?;
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

        self.call_state = CallState::Connecting;

        let options = RoomOptions::default();
        let (room, mut room_events) =
            Room::connect(&self.config.livekit_endpoint, &token, options).await?;

        info!("Connected to LiveKit room {}", room_name);

        // Save the room handle and info.
        self.current_user_name = Some(user_name.to_string());
        self.current_room_name = Some(room_name.to_string());
        self.current_room = Some(Arc::new(room));
        self.call_state = CallState::Connected;
        self.all_member_names = vec![user_name.to_string()];
        self.livekit_token = Some(token);

        // Start a task to handle room events.
        tokio::spawn(async move {
            while let Some(event) = room_events.recv().await {
                match event {
                    RoomEvent::ParticipantConnected(participant) => {
                        println!("Participant connected: {}", participant.identity());
                    }

                    RoomEvent::ActiveSpeakersChanged { speakers } => {
                        println!("Active speakers changed: {:?}", speakers);
                    }

                    RoomEvent::ParticipantDisconnected(participant) => {
                        println!("Participant disconnected: {}", participant.identity());
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
                        participant: _,
                    } => {
                        println!("Track subscribed: {:?}", track);

                        if let RemoteTrack::Audio(audio_track) = track {
                            // Play the audio_track.rtc_track in an async task using cpal.
                            play_audio_stream(audio_track).await.unwrap();
                        }
                    }

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
        });

        Ok(())
    }

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

async fn play_audio_stream(
    audio_track: RemoteAudioTrack,
) -> Result<(), Box<dyn std::error::Error>> {
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

    // Start a separate thread to handle the audio playback using cpal
    std::thread::spawn(move || {
        let stream = device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if let Ok(samples) = receiver.try_recv() {
                        info!("Received {} samples from cpal thread", samples.len());

                        for (i, sample) in samples.iter().enumerate() {
                            if i < data.len() {
                                data[i] = *sample;
                            }
                        }
                    } else {
                        // If no new samples, fill with silence.
                        info!("No new samples, filling with silence");
                        for sample in data.iter_mut() {
                            *sample = 0.0;
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

    // Push audio frames to the cpal stream asynchronously
    tokio::spawn(async move {
        loop {
            match audio_stream.next().await {
                Some(frame) => {
                    log::info!("Received audio frame with {} samples", frame.data.len());
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let voip_service = Arc::new(Mutex::new(MyVoipService::default()));

    // Create a channel to receive shutdown signal from the REPL and close the gRPC server.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // tokio::spawn({
    //     let voip_service = voip_service.clone();
    //     async move {
    //         repl::start_repl(voip_service, shutdown_tx).await;
    //     }
    // });

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
