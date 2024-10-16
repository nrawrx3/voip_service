use tonic::{transport::Server, Request, Response, Status};
use futures::Stream;
use std::pin::Pin;
use tokio_stream::wrappers::ReceiverStream;
use tokio::sync::mpsc;
use log::{info, warn};

mod repl;

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
#[derive(Debug, Default)]
pub struct MyVoipService {}

#[tonic::async_trait]
impl VoipService for MyVoipService {
    type RegisterStream = ServerEventStream;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::spawn(async {
        repl::start_repl().await;
    });

    // Define the server address.
    let addr = "[::1]:50051".parse().unwrap();
    let voip_service = MyVoipService::default();

    println!("Starting voip_service at {}", addr);

    // Start the gRPC server.
    Server::builder()
        .add_service(VoipServiceServer::new(voip_service))
        .serve(addr)
        .await?;

    Ok(())
}
