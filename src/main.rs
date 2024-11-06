use log::info;

mod cli_mode;
mod config;
mod logging;
mod model;
mod perf_samples;
mod voip_service;
mod wav;

#[cfg(feature = "windows-background-service")]
mod windows_service_mode;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "windows-background-service")]
    {
        info!("Starting in Windows service mode.");
        windows_service_mode::entry_main()?;
    }

    #[cfg(not(feature = "windows-background-service"))]
    {
        info!("Starting in CLI mode.");
        cli_mode::entry_main()?;
    }

    Ok(())
}
