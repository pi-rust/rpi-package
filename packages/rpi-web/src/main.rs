use rpi_web_extension::{
    config::{self, Launch},
    run,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match config::from_args(std::env::args().skip(1))? {
        Launch::Run(config) => run(*config).await?,
        Launch::Help => println!("{}", config::HELP),
        Launch::Version => println!("rpi-web {}", env!("CARGO_PKG_VERSION")),
        Launch::Initialized(path) => println!("Created {}", path.display()),
    }
    Ok(())
}
