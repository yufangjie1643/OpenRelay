#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("OPENRELAY_NO_TRAY").is_some() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(openrelay::server::run_from_env())?;
        return Ok(());
    }

    openrelay::tray::run()
}

#[cfg(not(windows))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    openrelay::server::run_from_env().await?;
    Ok(())
}
