#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|s| s == "--help") {
        println!("{}", rpi_http_extension::launch::HELP);
        return Ok(());
    }
    let (options, shutdown) = rpi_http_extension::launch::from_args(args)?;
    rpi_node_host::run_headless_with(options, shutdown).await?;
    Ok(())
}
