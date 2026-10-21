use rpi_web::{run, AppConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = AppConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("rpi-web {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--project" => config.project = args.next().ok_or("--project requires a path")?.into(),
            "--projects-root" => {
                config.projects_root =
                    Some(args.next().ok_or("--projects-root requires a path")?.into())
            }
            "--add-project" => config
                .projects
                .push(args.next().ok_or("--add-project requires a path")?.into()),
            "--assets" => {
                config.assets = Some(args.next().ok_or("--assets requires a path")?.into())
            }
            "--rpi-bin" => config.rpi_bin = args.next().ok_or("--rpi-bin requires a path")?.into(),
            "--bind" => config.bind = args.next().ok_or("--bind requires an address")?,
            "--port" => config.port = args.next().ok_or("--port requires a number")?.parse()?,
            "--session-dir" => {
                config.session_dir =
                    Some(args.next().ok_or("--session-dir requires a path")?.into())
            }
            "--registry" => {
                config.registry = args.next().ok_or("--registry requires a path")?.into()
            }
            "--help" | "-h" => {
                println!(
                    "rpi-web [--project PATH] [--rpi-bin PATH] [--bind IP] [--port N]
       [--add-project PATH]... [--projects-root PATH] [--assets PATH] [--session-dir PATH] [--registry PATH]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    run(config).await?;
    Ok(())
}
