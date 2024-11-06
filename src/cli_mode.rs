use log::info;
use std::sync::Arc;
use tokio::{runtime::Runtime, sync::Mutex};
use tonic::transport::Server;

use crate::voip_service::{pb::voip_service_server::VoipServiceServer, MyVoipService};

pub fn entry_main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let rt = Runtime::new().expect("Failed to create tokio runtime");
    info!("Tokio runtime created.");

    let (ctrlc_tx, mut ctrlc_rx) = tokio::sync::watch::channel(());

    ctrlc::set_handler(move || {
        info!("Received Ctrl-C signal. Shutting down...");
        let _ = ctrlc_tx.send(());
    })
    .expect("Error setting Ctrl-C handler");

    rt.block_on(async {
        let voip_service = Arc::new(Mutex::new(MyVoipService::default()));
        let addr = "[::1]:50051".parse().unwrap();
        info!("Starting grpc server at {}", addr);

        let result = Server::builder()
            .add_service(VoipServiceServer::new(voip_service.clone()))
            .serve_with_shutdown(addr, async {
                ctrlc_rx.changed().await.ok(); // Await the shutdown signal.

                // Drop all client event senders, which will close their streams.
                voip_service.lock().await.event_sender_of_client.clear();
            })
            .await;

        match result {
            Ok(_) => info!("Server shutdown successfully"),
            Err(e) => info!("Server shutdown with error: {:?}", e),
        }
    });

    Ok(())
}
