mod model;
mod wav;

mod perf_samples;

mod config;

use logging::setup_logging;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use voip_service::MyVoipService;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use log::info;
use tokio::sync::{oneshot, Mutex};
use tonic::transport::Server;
use voip_service::pb::voip_service_server::VoipServiceServer;

mod logging;
mod voip_service;

// Service entry point
define_windows_service!(ffi_service_main, my_service_main);

fn my_service_main(_arguments: Vec<OsString>) -> Result<(), windows_service::Error> {
    setup_logging().expect("Failed to initialize logger");
    info!("Service main function has started.");

    // Create the oneshot channel for signaling shutdown
    let (shutdown_from_scm_tx, shutdown_from_scm_rx) = oneshot::channel::<()>();

    // Wrap the sender in a sync mutex so it can be moved into the service control handler. Some Rustonomics at play here.
    let shutdown_signal = Arc::new(std::sync::Mutex::new(Some(shutdown_from_scm_tx)));

    // Register the service control handler
    let shutdown_signal_clone = Arc::clone(&shutdown_signal);
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop => {
                info!("Received stop control event from SCM.");
                if let Some(tx) = shutdown_signal_clone.lock().unwrap().take() {
                    let _ = tx.send(()); // Send the shutdown signal only once
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => {
                info!(
                    "Received unhandled control event from SCM: {:?}",
                    control_event
                );
                ServiceControlHandlerResult::NotImplemented
            }
        }
    };

    let status_handle = service_control_handler::register("voipservice", event_handler)?;
    info!("Service control handler registered.");

    // Notify SCM that the service is starting
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(1),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;
    info!("Service status set to START_PENDING.");

    // Initialize the Tokio runtime
    let rt = Runtime::new().expect("Failed to create Tokio runtime");
    info!("Tokio runtime created.");

    rt.block_on(async {
        info!("Entered async block.");

        // Notify SCM that the service is now running
        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::Running,
                controls_accepted: ServiceControlAccept::STOP,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })
            .unwrap();
        info!("Service status set to RUNNING.");

        let voip_service = Arc::new(Mutex::new(MyVoipService::default()));
        let addr = "[::1]:50051".parse().unwrap();
        info!("Starting grpc server at {}", addr);

        let result = Server::builder()
            .add_service(VoipServiceServer::new(voip_service.clone()))
            .serve_with_shutdown(addr, async {
                shutdown_from_scm_rx.await.ok(); // Await the shutdown signal
                info!("Shutting down grpc server");

                // Drop all client event senders, which will close their streams
                voip_service.lock().await.event_sender_of_client.clear();
            })
            .await;

        match result {
            Ok(_) => info!("Server shutdown successfully"),
            Err(e) => info!("Server shutdown with error: {:?}", e),
        }
    });

    info!("Service is shutting down, setting status to STOPPED.");

    // Set service status to stopped before exiting
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;
    info!("Service has been stopped.");

    Ok(())
}

fn main() -> Result<(), windows_service::Error> {
    // Register generated `ffi_service_main` with the system and start the service, blocking
    // this thread until the service is stopped.
    service_dispatcher::start("voipservice", ffi_service_main)?;
    info!("Exiting main");
    Ok(())
}
