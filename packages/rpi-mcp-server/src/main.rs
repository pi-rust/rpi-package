use rpi_mcp_server::{run, serve_http, HttpOptions, ServerOptions};
use rpi_plugin_sdk::stdio::{InputLine, StdinLines};
use std::path::PathBuf;
use tokio::sync::watch;

const USAGE: &str = "\
rpi-mcp-bridge - MCP server for rpi

USAGE:
    rpi-mcp-bridge [OPTIONS]              Serve MCP on stdin/stdout
    rpi-mcp-bridge --http <ADDR> [OPTIONS]  Serve MCP Streamable HTTP

The bridge drives a child `rpi --mode rpc` host for the actual agent work.
Diagnostics are written to stderr only; stdout carries stdio protocol frames.

OPTIONS:
    --cwd <DIR>             Workspace for the agent (default: current directory)
    --model <MODEL>         Model forwarded to the native host
    --provider <PROVIDER>   Provider forwarded to the native host
    --session-dir <DIR>     Native session directory (default: native default)
    --resume <FILE>         Continue an existing native session file
    --bypass                Disable permission-gate approvals (trusted use only)

HTTP OPTIONS (with --http):
    --http <ADDR>           Bind Streamable HTTP, for example 127.0.0.1:8080
    --token <TOKEN>         Require `Authorization: Bearer <TOKEN>`
    --allow-origin <VALUE>  Extra allowed Origin/Host value; repeatable
    --max-sessions <N>      Concurrent MCP sessions (default 8)

    -h, --help              Show this help
    -V, --version           Show this version

Non-loopback --http binds require --token.
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
    let cli = match cli_from_args(&args) {
        Ok(cli) => cli,
        Err(error) => {
            eprintln!("[rpi-mcp] {error}");
            std::process::exit(2);
        }
    };
    let code = match cli.http {
        Some(http) => match serve_http(http, cli.server).await {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("[rpi-mcp] {error}");
                1
            }
        },
        None => serve_stdio(cli.server).await,
    };
    std::process::exit(code);
}

struct Cli {
    server: ServerOptions,
    http: Option<HttpOptions>,
}

async fn serve_stdio(options: ServerOptions) -> i32 {
    // Probe stdin once so a terminal fails before the native child starts. The
    // probe may already hold lines, so the same reader is handed to the server.
    let mut input = StdinLines::default();
    let mut pending = None;
    match input.poll_line() {
        Ok(InputLine::Line(line)) => pending = Some(line),
        Ok(InputLine::Eof | InputLine::Pending) => {}
        Err(error) => {
            eprintln!("[rpi-mcp] stdin is not a pipe: {error}");
            return 2;
        }
    }
    let (stop, receiver) = watch::channel(false);
    let code = run(input, pending, receiver, options).await;
    let _ = stop.send(true);
    code
}

fn cli_from_args(args: &[String]) -> Result<Cli, String> {
    let mut server = ServerOptions::default();
    let mut bind: Option<String> = None;
    let mut token: Option<String> = None;
    let mut allow_origins = Vec::new();
    let mut max_sessions = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--bypass" => server.bypass = true,
            "--cwd" => server.cwd = PathBuf::from(value(args, &mut index, "--cwd")?),
            "--model" => server.model = Some(value(args, &mut index, "--model")?),
            "--provider" => server.provider = Some(value(args, &mut index, "--provider")?),
            "--session-dir" => {
                server.session_dir = Some(PathBuf::from(value(args, &mut index, "--session-dir")?))
            }
            "--resume" => {
                server.saved_session = Some(PathBuf::from(value(args, &mut index, "--resume")?))
            }
            "--http" => bind = Some(value(args, &mut index, "--http")?),
            "--token" => token = Some(value(args, &mut index, "--token")?),
            "--allow-origin" => allow_origins.push(value(args, &mut index, "--allow-origin")?),
            "--max-sessions" => {
                let raw = value(args, &mut index, "--max-sessions")?;
                let parsed: usize = raw
                    .parse()
                    .ok()
                    .filter(|count| (1..=64).contains(count))
                    .ok_or_else(|| "--max-sessions must be an integer from 1 to 64".to_string())?;
                max_sessions = Some(parsed);
            }
            other => return Err(format!("unknown option `{other}` (try --help)")),
        }
        index += 1;
    }
    let http = match bind {
        Some(bind) => {
            let mut options = HttpOptions::new(bind);
            options.token = token;
            options.allow_origins = allow_origins;
            if let Some(max_sessions) = max_sessions {
                options.max_sessions = max_sessions;
            }
            Some(options)
        }
        None => {
            if token.is_some() || !allow_origins.is_empty() || max_sessions.is_some() {
                return Err("--token, --allow-origin and --max-sessions need --http".into());
            }
            None
        }
    };
    Ok(Cli { server, http })
}

fn value(args: &[String], index: &mut usize, name: &str) -> Result<String, String> {
    let next = args
        .get(*index + 1)
        .filter(|value| !value.trim().is_empty() && !value.starts_with("--"))
        .ok_or_else(|| format!("{name} needs a value"))?;
    *index += 1;
    Ok(next.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn parses_server_options() {
        let cli = cli_from_args(&args(&[
            "--cwd",
            "/tmp/work",
            "--model",
            "provider/model",
            "--provider",
            "provider",
            "--session-dir",
            "/tmp/sessions",
            "--resume",
            "/tmp/sessions/one.jsonl",
            "--bypass",
        ]))
        .unwrap();
        assert!(cli.http.is_none());
        assert_eq!(cli.server.cwd, PathBuf::from("/tmp/work"));
        assert_eq!(cli.server.model.as_deref(), Some("provider/model"));
        assert_eq!(cli.server.provider.as_deref(), Some("provider"));
        assert_eq!(cli.server.session_dir, Some(PathBuf::from("/tmp/sessions")));
        assert_eq!(
            cli.server.saved_session,
            Some(PathBuf::from("/tmp/sessions/one.jsonl"))
        );
        assert!(cli.server.bypass);
    }

    #[test]
    fn parses_http_options() {
        let cli = cli_from_args(&args(&[
            "--http",
            "127.0.0.1:8080",
            "--token",
            "secret",
            "--allow-origin",
            "https://ide.example",
            "--allow-origin",
            "https://second.example",
            "--max-sessions",
            "3",
        ]))
        .unwrap();
        let http = cli.http.expect("http mode");
        assert_eq!(http.bind, "127.0.0.1:8080");
        assert_eq!(http.token.as_deref(), Some("secret"));
        assert_eq!(http.allow_origins.len(), 2);
        assert_eq!(http.max_sessions, 3);
    }

    #[test]
    fn rejects_missing_values_unknown_options_and_orphan_http_flags() {
        assert!(cli_from_args(&args(&["--cwd"])).is_err());
        assert!(cli_from_args(&args(&["--cwd", "--model"])).is_err());
        assert!(cli_from_args(&args(&["--nope"])).is_err());
        assert!(cli_from_args(&args(&["--token", "secret"])).is_err());
        assert!(cli_from_args(&args(&["--http", "127.0.0.1:1", "--max-sessions", "0"])).is_err());
    }
}
