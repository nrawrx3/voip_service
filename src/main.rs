use log::info;

mod cli_mode;
mod config;
mod debug_stats;
mod device_config;
mod logging;
mod model;
mod voip_service;

#[cfg(feature = "windows-background-service")]
mod windows_service_mode;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(feature = "windows-background-service") {
        info!("Starting in Windows service mode.");
        #[cfg(feature = "windows-background-service")]
        windows_service_mode::entry_main()?;
    } else if cfg!(feature = "cli-mode") {
        info!("Starting in CLI mode.");
        #[cfg(feature = "cli-mode")]
        cli_mode::entry_main()?;
    } else {
        info!("No valid mode selected. Please enable either 'windows-background-service' or 'cli-mode' feature.");
    }

    Ok(())
}
