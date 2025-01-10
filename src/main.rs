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
    #[cfg(feature = "windows-background-service")]
    {
        info!("Starting in Windows service mode.");
        windows_service_mode::entry_main()?;
    }

    #[cfg(feature = "cli-mode")]
    {
        info!("Starting in CLI mode.");
        cli_mode::entry_main()?;
    }

    Ok(())
}
