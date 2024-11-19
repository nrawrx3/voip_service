use log::info;
use std::sync::Arc;
use tokio::{runtime::Runtime, sync::Mutex, task::LocalSet};
use tonic::transport::Server;

use crate::{
    debug_stats::SharedDebugStats,
    voip_service::{
        pb::voip_service_server::VoipServiceServer, should_stop_frame_poller, start_audio_playback,
        MyVoipService,
    },
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

    let debug_stats =
        SharedDebugStats::new(std::sync::Mutex::new(crate::debug_stats::DebugStats {
            unique_audio_packet_configs: std::collections::HashSet::new(),
        }));

    let debug_stats_clone = debug_stats.clone();

    rt.block_on(async {
        let voip_service = Arc::new(Mutex::new(MyVoipService::default()));

        let vs_clone = voip_service.clone();

        let local_set = LocalSet::new();
        local_set
            .spawn_local(async move { start_audio_playback(vs_clone, debug_stats_clone).await });

        let addr = "[::1]:50051".parse().unwrap();
        info!("Starting grpc server at {}", addr);

        let grpc_server_future = tokio::spawn(async move {
            Server::builder()
                .add_service(VoipServiceServer::new(voip_service.clone()))
                .serve_with_shutdown(addr, async move {
                    ctrlc_rx.changed().await.ok(); // Await the shutdown signal.

                    // Drop all client event senders, which will close their streams.
                    voip_service.lock().await.event_sender_of_client.clear();

                    should_stop_frame_poller.store(true, std::sync::atomic::Ordering::Release);

                    // Print the debug stats.
                    debug_stats.lock().unwrap().print();
                })
                .await
                .expect("Error shutting down grpc server");
        });

        let result = tokio::join!(grpc_server_future, local_set);
        // result.1.expect("Error closing tasks");

        info!("Audio playback task finished.");
    });

    Ok(())
}
