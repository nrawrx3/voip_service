use log::info;
use ringbuf::rb::local;
use std::sync::Arc;
use tokio::{runtime::Runtime, sync::Mutex, task::LocalSet};
use tonic::transport::Server;

use crate::voip_service::{
    pb::voip_service_server::VoipServiceServer, start_audio_playback, MyVoipService,
};

pub fn entry_main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    console_subscriber::init();

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

        let vs_clone = voip_service.clone();

        let local_set = LocalSet::new();
        local_set.spawn_local(async move { start_audio_playback(vs_clone).await });

        let addr = "[::1]:50051".parse().unwrap();
        info!("Starting grpc server at {}", addr);

        let grpc_server_future = tokio::spawn(async move {
            Server::builder()
                .add_service(VoipServiceServer::new(voip_service.clone()))
                .serve_with_shutdown(addr, async move {
                    ctrlc_rx.changed().await.ok(); // Await the shutdown signal.

                    // Drop all client event senders, which will close their streams.
                    voip_service.lock().await.event_sender_of_client.clear();
                })
                .await
                .expect("Error shutting down grpc server");
        });

        // Not joining on the frame poller local. A bit hard to do and not
        // really needed. We will simply exit the process when the grpc server
        // exits.
        let result = tokio::join!(grpc_server_future);
        // result.1.expect("Error closing tasks");

        info!("Audio playback task finished.");
    });

    Ok(())
}
