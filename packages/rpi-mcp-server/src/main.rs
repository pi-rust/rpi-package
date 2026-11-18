use rpi_mcp_server::{run, ServerOptions};
use rpi_plugin_sdk::stdio::{InputLine, StdinLines};
use std::path::PathBuf;
use tokio::sync::watch;

const USAGE: &str = "\
rpi-mcp-bridge - MCP stdio server for rpi

USAGE:
    rpi-mcp-bridge [OPTIONS]

The bridge speaks MCP JSON-RPC 2.0 over stdin/stdout, one frame per line, and
drives a child `rpi --mode rpc` host for the actual agent work. Diagnostics are
written to stderr only.

OPTIONS:
    --cwd <DIR>            Workspace for the agent (default: current directory)
    --model <MODEL>        Model forwarded to the native host
    --provider <PROVIDER>  Provider forwarded to the native host
    --session-dir <DIR>    Native session directory (default: native default)
    --bypass               Disable permission-gate approvals (trusted use only)
    -h, --help             Show this help
    -V, --version          Show this version
";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{USAGE}");
        return;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("rpi-mcp-bridge {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let options = match options_from_args(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("[rpi-mcp] {error}");
            std::process::exit(2);
        }
    };
    // Probe stdin once so a terminal fails before the native child starts. The
    // probe may already hold lines, so the same reader is handed to the server.
    let mut input = StdinLines::default();
    let mut pending = None;
    match input.poll_line() {
        Ok(InputLine::Line(line)) => pending = Some(line),
        Ok(InputLine::Eof) => {}
        Ok(InputLine::Pending) => {}
        Err(error) => {
            eprintln!("[rpi-mcp] stdin is not a pipe: {error}");
            std::process::exit(2);
        }
    }
    let (stop, receiver) = watch::channel(false);
    let code = run(input, pending, receiver, options).await;
    let _ = stop.send(true);
    std::process::exit(code);
}

fn options_from_args(args: &[String]) -> Result<ServerOptions, String> {
    let mut options = ServerOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--bypass" => options.bypass = true,
            "--cwd" => options.cwd = PathBuf::from(value(args, &mut index, "--cwd")?),
            "--model" => options.model = Some(value(args, &mut index, "--model")?),
            "--provider" => options.provider = Some(value(args, &mut index, "--provider")?),
            "--session-dir" => {
                options.session_dir = Some(PathBuf::from(value(args, &mut index, "--session-dir")?))
            }
            other => return Err(format!("unknown option `{other}` (try --help)")),
        }
        index += 1;
    }
    Ok(options)
}

fn value(args: &[String], index: &mut usize, name: &str) -> Result<String, String> {
    let next = args
        .get(*index + 1)
        .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
        .ok_or_else(|| format!("{name} needs a value"))?;
    *index += 1;
    Ok(next.clone())
}
