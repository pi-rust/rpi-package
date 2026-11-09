use rpi_httpcore::config::{self, Launch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match config::from_args(std::env::args().skip(1))? {
        Launch::Help => println!("{}", config::HELP),
        Launch::Version => println!("rpi-httpcore {}", env!("CARGO_PKG_VERSION")),
        Launch::Run(config) => rpi_httpcore::run(*config).await?,
    }
    Ok(())
}
