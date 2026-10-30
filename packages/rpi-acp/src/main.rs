//! Standalone `rpi-acp` bridge — run the ACP server in its own process.
//!
//! This is the same server the cdylib extension runs ([`rpi_acp::protocol::run`]),
//! but launched directly by an ACP client (Zed, or anything else) instead of
//! being embedded in an `rpi` host that has to yield its stdio.
//!
//! Practical consequences:
//!
//! * It owns stdin/stdout natively, so it does **not** need the host's
//!   `ownsStdio` capability — it works with a stock `rpi`.
//! * It drives child `rpi --mode rpc` processes for the actual agent work.
//!   `rpi` is located via `RPI_BIN`, a sibling `rpi` next to this binary, or
//!   `rpi` on `PATH` (see [`rpi_acp::session`]).
//!
//! Besides serving, it can run the maintenance commands an ACP client or a
//! package manager needs:
//!
//! ```text
//! rpi-acp-bridge --setup           # provider credential login (ACP terminal auth)
//! rpi-acp-bridge --install-zed     # add agent_servers.rpi, idempotent
//! rpi-acp-bridge --print-zed       # print the settings snippet, write nothing
//! ```
//!
//! Zed configuration (what `--install-zed` writes):
//!
//! ```jsonc
//! { "agent_servers": { "rpi": { "type": "custom", "command": "rpi-acp-bridge", "args": [] } } }
//! ```

use rpi_acp::diagln;

const USAGE: &str = "\
rpi-acp-bridge - ACP stdio agent for rpi

USAGE:
    rpi-acp-bridge [--bypass]
    rpi-acp-bridge --experimental-http [--bind <IP:PORT>] [--allow-origin <ORIGIN>]
    rpi-acp-bridge --experimental-remote [--bind <IP:PORT>] [--allow-origin <ORIGIN>]
    rpi-acp-bridge [--setup [--provider <ID>]] [--install-zed [OPTIONS]] [--print-zed]

With no arguments it serves ACP on stdin/stdout (this is how Zed launches it).
Remote mode serves WebSocket only at /acp. --experimental-http instead serves
HTTP/2 Streamable HTTP at /acp (explicit opt-in; no live event resumption).
It requires RPI_ACP_REMOTE_TOKEN (32+ printable ASCII bytes), defaults to
127.0.0.1:9231, and denies browser Origins unless exactly allowed. Use a trusted
TLS reverse proxy for non-loopback access. The proxy must preserve the listener
Host header. Remote bearer access grants full agent privileges. Disconnect
cancels live work; reconnect with initialize and session/load for saved history.
Experimental limits: 16 connections, 1 MiB/message, 256 text messages and
8 session opens per connection, 10s initial/120s input idle/1h lifetime.

COMMANDS:
    --bypass        Serve ACP without permission-gate approvals
    --setup         Log in to a provider, then exit 0 once credentials are usable.
                    This is the ACP `terminal` auth method: an ACP client runs
                    the same command it uses to start the agent, plus --setup.
                    Must be interactive. A credential already in the environment
                    or in the rpi agent dir needs no prompt (exit 0 immediately);
                    otherwise it runs `rpi auth login` and re-checks afterwards.
    --provider <ID> Provider used by ACP credential checks and agent login
                    (default: anthropic); may be combined with --bypass
    --install-zed   Register rpi under `agent_servers` in Zed's settings.json
    --print-zed     Print the settings snippet instead of writing it
    -h, --help      Show this help
    -V, --version   Show the version

OPTIONS (for --setup):
    --provider <ID>     Provider to log in to (default: anthropic)

OPTIONS (for --install-zed):
    --settings <FILE>   Zed settings.json (default: Zed's per-OS location,
                        or $ZED_SETTINGS)
    --command <FILE>    Executable to register (default: this binary)
    --dry-run           Report what would change, write nothing
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--setup` is the ACP terminal-auth entry point and may arrive with extra
    // flags after it (`--setup --provider openai`), so it is detected anywhere
    // in the argument list rather than only in first position.
    if rpi_acp::auth::requested(&args) {
        std::process::exit(rpi_acp::auth::run(&args));
    }
    match subcommand(args) {
        Some(code) => std::process::exit(code),
        None => serve(),
    }
}

/// Handle the non-server command surface.
///
/// `Some(code)` means a command ran and the process should exit with `code`;
/// `None` falls through to the ACP server so an ACP client can spawn us bare.
fn subcommand(args: Vec<String>) -> Option<i32> {
    if args.iter().any(|arg| arg == "--provider" || arg.starts_with("--provider=")) {
        // These remain in process argv for auth::configured_provider; only
        // validate the server option surface here. Never print provider input.
        let mut provider = false;
        let mut bypass = false;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--bypass" if !bypass => bypass = true,
                "--provider" if !provider => {
                    index += 1;
                    if args.get(index).is_none_or(|value| value.trim().is_empty() || value.starts_with('-')) {
                        return Some(fail("--provider needs a provider ID"));
                    }
                    provider = true;
                }
                value if value.starts_with("--provider=") && !provider => {
                    if value[11..].trim().is_empty() { return Some(fail("--provider needs a provider ID")); }
                    provider = true;
                }
                _ => return Some(fail("invalid server options (try --help)")),
            }
            index += 1;
        }
        if bypass { rpi_acp::session::set_permission_bypass(true); }
        return None;
    }
    match args.first().map(String::as_str)? {
        "--experimental-remote" => Some(serve_remote(&args[1..], false)),
        "--experimental-http" => Some(serve_remote(&args[1..], true)),
        "--bypass" if args.len() == 1 => {
            rpi_acp::session::set_permission_bypass(true);
            None
        }
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            Some(0)
        }
        "--version" | "-V" | "version" => {
            println!("rpi-acp-bridge {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        "--print-zed" | "print-zed" => {
            println!("{}", rpi_acp::zed::Registration::detect().snippet());
            Some(0)
        }
        "--install-zed" | "install-zed" => Some(install_zed(&args[1..])),
        _ => Some(fail("unknown command (try --help)")),
    }
}

fn install_zed(rest: &[String]) -> i32 {
    let mut settings: Option<std::path::PathBuf> = None;
    let mut command: Option<String> = None;
    let mut dry_run = false;
    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "--dry-run" => dry_run = true,
            "--settings" => match rest.get(index + 1) {
                Some(value) => {
                    settings = Some(std::path::PathBuf::from(value));
                    index += 1;
                }
                None => return fail("--settings needs a path"),
            },
            "--command" => match rest.get(index + 1) {
                Some(value) => {
                    command = Some(value.clone());
                    index += 1;
                }
                None => return fail("--command needs a path"),
            },
            other => return fail(&format!("unknown option `{other}` (try --help)")),
        }
        index += 1;
    }

    let path = match settings.or_else(rpi_acp::zed::settings_path) {
        Some(path) => path,
        None => return fail("cannot locate Zed's settings.json; pass --settings <FILE>"),
    };

    let mut registration = rpi_acp::zed::Registration::detect();
    if let Some(command) = command {
        registration = rpi_acp::zed::Registration {
            command,
            args: registration.args,
        };
    }

    match rpi_acp::zed::register_file(&path, &registration, dry_run) {
        Ok(outcome) => {
            println!("{}", rpi_acp::zed::describe(&outcome, &path, dry_run));
            if !dry_run {
                if let rpi_acp::zed::Outcome::Updated {
                    backup: Some(backup),
                } = &outcome
                {
                    println!("backup: {}", backup.display());
                }
            }
            0
        }
        Err(error) => fail(&error),
    }
}

fn fail(message: &str) -> i32 {
    diagln!("error: {message}");
    1
}

fn serve() {
    // Fail fast when there is no ACP client on stdin, with a hint instead of the
    // bare OS error: running the bridge from a terminal is a common mistake.
    let (input, pending) = match rpi_acp::protocol::acp_stdin() {
        Ok(taken) => taken,
        Err(error) => {
            diagln!("error: {error}");
            diagln!(
                "hint: the bridge speaks ACP on stdin, so it must be started by an ACP \
                 client (Zed). To register it in Zed, run: rpi-acp-bridge --install-zed"
            );
            std::process::exit(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            diagln!("[rpi-acp] failed to create tokio runtime: {error}");
            std::process::exit(2);
        }
    };

    // `run` returns when the client closes stdin; exit the process with its code.
    // The standalone bridge has no host to signal shutdown, so keep the sender
    // alive: EOF on stdin is the only stop condition.
    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    std::process::exit(runtime.block_on(rpi_acp::protocol::run(stop_rx, input, pending)));
}

fn serve_remote(args: &[String], http: bool) -> i32 {
    let mut bind: std::net::SocketAddr = "127.0.0.1:9231".parse().unwrap();
    let mut origin = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--bind" => match args.get(index + 1).and_then(|value| value.parse().ok()) {
                Some(value) => bind = value,
                None => return fail("--bind needs a literal IP:PORT"),
            },
            "--allow-origin" => match args.get(index + 1) {
                Some(value) => origin = Some(value.clone()),
                None => return fail("--allow-origin needs an exact origin"),
            },
            _ => return fail("unknown remote option (try --help)"),
        }
        index += 2;
    }
    let token = match std::env::var("RPI_ACP_REMOTE_TOKEN") {
        Ok(token) => token,
        Err(_) => return fail("remote mode requires RPI_ACP_REMOTE_TOKEN"),
    };
    let config = match rpi_acp::transport::Config::new(&token, origin) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };
    drop(token);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return fail("cannot create remote runtime"),
    };
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(_) => return fail("cannot bind remote listener"),
        };
        diagln!("[rpi-acp] EXPERIMENTAL {} listener on {bind}/acp", if http { "HTTP/2" } else { "WebSocket-only" });
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let signal = tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = stop.send(true);
        });
        let result = if http {
            rpi_acp::transport::http::serve(listener, config, receiver).await
        } else {
            rpi_acp::transport::serve(listener, config, receiver).await
        };
        signal.abort();
        match result {
            Ok(()) => 0,
            Err(_) => fail("remote listener failed"),
        }
    })
}
