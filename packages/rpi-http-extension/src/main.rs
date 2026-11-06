//! Standalone entry point: the same API-only HTTP service the plugin starts.

use rpi_http::config::{self, Launch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match config::from_args(std::env::args().skip(1))? {
        Launch::Help => println!("{}", config::HELP),
        Launch::Version => println!("rpi-http-extension {}", env!("CARGO_PKG_VERSION")),
        Launch::Run(config) => rpi_http::run(*config).await?,
    }
    Ok(())
}
