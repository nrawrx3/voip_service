use livekit::{Room, RoomEvent, RoomOptions};
use tonic::{transport::Server, Request, Response, Status};
use tokio::sync::Mutex;
use std::sync::Arc;
use futures::Stream;
use std::fmt::format;
use std::pin::Pin;
use tokio_stream::wrappers::ReceiverStream;
use tokio::sync::mpsc;
use log::{error, info, warn};
use reqwest::{Client, StatusCode};

mod repl;

mod config;
use config::Config;

pub mod pb {
    // tonic::include!("/pb/voip.rs"); // Generated from the proto file
    include!("pb/voip.rs");
}

use pb::voip_service_server::{VoipService, VoipServiceServer};
use pb::{ClientInfo, VoipServerEvent};
use pb::voip_server_event::{EventType, EventPayload};

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
}

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
        }
    }
}

#[tonic::async_trait]
impl VoipService for MyVoipService {
    type RegisterStream = ServerEventStream;

    // TODO: Just a dummy as of now.
    async fn register(
        &self,
        request: Request<ClientInfo>,
    ) -> Result<Response<Self::RegisterStream>, Status> {
        let client_info = request.into_inner();
        println!(
            "Client {} registered for room {}",
            client_info.client_id, client_info.room_id
        );

        // Create a channel to send events to the client.
        let (tx, rx) = mpsc::channel(10);

        // Simulate pushing events to the client in a background task.
        tokio::spawn(async move {
            let mut counter = 0;
            loop {
                let (event_type, event_payload) = match counter % 5 {
                    0 => (EventType::MemberJoined, EventPayload::MemberJoined(pb::MemberJoined {
                        member_id: "lawda".to_string(),
                    })),
                    1 => (EventType::MemberLeft, EventPayload::MemberLeft(pb::MemberLeft {
                        member_id: "lawda".to_string(),
                    })),
                    2 => (EventType::MemberMuted, EventPayload::MemberMuted(pb::MemberMuted {
                        member_id: "lawda".to_string(),
                    })),
                    3 => (EventType::MemberUnmuted, EventPayload::MemberUnmuted(pb::MemberUnmuted {
                        member_id: "lawda".to_string(),
                    })),
                    _ => (EventType::MemberSpeaking, EventPayload::MemberSpeaking(pb::MemberSpeaking {
                        member_id: "lawda".to_string(),
                    })),
                };

                let event = VoipServerEvent {
                    event_type: event_type as i32,
                    current_user_id: client_info.client_id.clone(),
                    room_id: client_info.room_id.clone(),
                    event_payload: Some(event_payload),
                };

                if tx.send(Ok(event)).await.is_err() {
                    warn!("Client {} disconnected", client_info.client_id);
                    break; // Client disconnected, stop sending events.
                }

                counter += 1;
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        });

        // Convert the receiver into a stream.
        let stream = ReceiverStream::new(rx);

        Ok(Response::new(Box::pin(stream) as Self::RegisterStream))
    }
}

impl MyVoipService {
    pub async fn connect_to_livekit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.current_user_name.is_none() || self.current_room_name.is_none() {
            warn!("Cannot connect to LiveKit without knowing the user name and room name");
            return Err("Not logged in to livekit".into());
        }

        let user_name = self.current_user_name.as_ref().unwrap();
        let room_name = self.current_room_name.as_ref().unwrap();

        info!("Fetching join token for self {} in room {}", user_name, room_name);

        let url = reqwest::Url::parse_with_params(&self.config.livekit_token_endpoint, &[("room", room_name), ("identity", user_name)])?;

        info!("Fetching join token from {}", url);

        let resp = self.client.get(url).send().await?;
        if !matches!(resp.status(), StatusCode::OK) {
            error!("Failed to get livekit token for room {}. HTTP Error: {}", room_name, resp.status());
            return Err("Failed to get livekit token".into());
        }

        let token = resp.text().await?;

        info!("Received token {}", token);

        self.call_state = CallState::Connecting;

        let options = RoomOptions::default();
        let (room, mut room_events) = Room::connect(
            &self.config.livekit_endpoint,
            &token,
            options,
        ).await?;

        info!("Connected to LiveKit room {}", room_name);

        // Save the room handle.
        self.current_room = Some(Arc::new(room));
        self.call_state = CallState::Connected;

        self.all_member_names = vec![user_name.to_string()];
        self.livekit_token = Some(token);

        tokio::spawn(async move {
            while let Some(event) = room_events.recv().await {
                match event {
                    RoomEvent::ParticipantConnected(participant) => {
                        info!("Participant connected: {}", participant.identity());
                    }

                    RoomEvent::ActiveSpeakersChanged { speakers } => {
                        info!("Active speakers changed: {:?}", speakers);
                    }

                    RoomEvent::ParticipantDisconnected(participant) => {
                        info!("Participant disconnected: {}", participant.identity());
                    }

                    RoomEvent::LocalTrackPublished { publication, track, participant } => {
                        info!("Local track published: {:?}", publication);
                    }

                    _ => {
                        info!("Unhandled room event: {:?}", event);
                    }
                }
            }
        });

        Ok(())
    }
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let voip_service = Arc::new(Mutex::new(MyVoipService::default()));

    // Create a channel to receive shutdown signal from the REPL and close the gRPC server.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn({
        let voip_service = voip_service.clone();
        async move {
            repl::start_repl(voip_service, shutdown_tx).await;
        }
    });

    let addr = "[::1]:50051".parse().unwrap();
    let svc = MyVoipService::default();

    info!("Starting voip_service at {}", addr);

    Server::builder()
        .add_service(VoipServiceServer::new(svc))
        .serve_with_shutdown(addr, async {
            shutdown_rx.await.ok();
        })
        .await?;

    Ok(())
}