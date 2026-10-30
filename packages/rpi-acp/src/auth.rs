//! Credential setup for ACP's `terminal` authentication method.
//!
//! The ACP Registry only lists agents whose `initialize` response advertises an
//! `agent` or `terminal` auth method (see the ACP auth-methods RFD), and it
//! verifies the `terminal` one by launching the *same* configured agent program
//! with the descriptor's `args`. So `rpi-acp-bridge --setup` (and
//! `rpi --acp --setup`) must be a real login that exits `0` once credentials are
//! usable — not a message telling the user to run something else.
//!
//! Setup reuses rpi's own credential handling rather than reimplementing it, and
//! never invents a success:
//!
//! 1. `rpi auth check --provider <id> --json` — no network call, no prompt. An
//!    environment variable or an existing `~/.rpi` credential is already enough
//!    and exits `0` without opening a prompt at all (the non-interactive path the
//!    registry's verifier takes).
//! 2. Otherwise `rpi auth login --provider <id>` runs **only when stdin is a
//!    terminal**: its prompt is TTY-only, and running it on a pipe would hang
//!    until some outer timeout kills it. On a pipe we fail fast with the exact
//!    commands that work instead.
//! 3. `rpi auth check` again decides the exit code, so a zero exit from `login`
//!    alone never claims success.
//!
//! Credentials land in rpi's own agent directory (`~/.rpi` by default, or
//! whatever `RPI_CODING_AGENT_DIR` points at in a container), so the ACP server
//! that starts afterwards — in a new process, with a clean environment — finds
//! them without any extra plumbing.

use serde_json::Value;
use std::io::IsTerminal;

use crate::diagln;

/// Argument the ACP `terminal` auth descriptor appends to the agent program.
pub const SETUP_ARG: &str = "--setup";
/// Auth method id advertised in `initialize`.
pub const AUTH_METHOD_ID: &str = "rpi-credentials";
/// Provider `rpi auth login` defaults to; also what the registry's verifier gets
/// when it launches us with no extra arguments.
pub const DEFAULT_PROVIDER: &str = "anthropic";

/// Whether this invocation is the auth setup rather than an ACP server.
///
/// Accepted anywhere in the argument list, so an ACP client that also forwards
/// its own flags (`--setup --provider openai`) still lands here.
pub fn requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == SETUP_ARG || arg == "setup")
}

/// Run the setup flow for `--setup`; returns the process exit code.
///
/// Exit status is the only success signal ACP defines for terminal auth, so `0`
/// means "credentials verified usable" and nothing else.
pub fn run(args: &[String]) -> i32 {
    let provider = match provider_arg(args) {
        Ok(provider) => provider.unwrap_or_else(|| DEFAULT_PROVIDER.to_string()),
        Err(message) => {
            diagln!("error: {message}");
            return 2;
        }
    };
    setup(&provider)
}

/// Set up credentials for `provider` (a stored or environment credential
/// already counts), returning the exit code ACP's terminal auth reads.
pub fn setup(provider: &str) -> i32 {
    // Step 1: an existing credential — an environment variable or a previous
    // login — is the whole job, and needs no prompt. This is the path the
    // registry's verifier takes, with the sandbox HOME it hands us.
    match check(provider) {
        Ok(Verdict::Ready { source }) => {
            println!("rpi: {provider} credentials are already available ({source}).");
            return 0;
        }
        Ok(Verdict::Missing) => {}
        Err(error) => {
            diagln!("error: could not read credential state for {provider}: {error}");
            return 1;
        }
    }

    // Step 2: log in. `rpi auth login` reads stdin through a terminal only, so
    // this is the one thing we cannot do non-interactively; a piped run exits
    // with instructions rather than hanging until a client gives up on us.
    if !std::io::stdin().is_terminal() {
        diagln!("error: no {provider} credential is configured here.");
        diagln!("       Run this in a terminal to log in interactively:");
        diagln!("         rpi-acp-bridge {SETUP_ARG} --provider {provider}");
        diagln!("       Non-interactive alternatives that need no prompt:");
        diagln!("         export ANTHROPIC_API_KEY=...   # or OPENAI_API_KEY for openai-*");
        diagln!("         rpi auth login --provider <id>  # in a terminal");
        return 3;
    }

    match login(provider) {
        Ok(code) if code != 0 => {
            diagln!("error: `rpi auth login --provider {provider}` exited {code}");
            1
        }
        // Step 3: trust only the follow-up check, never `login`'s own exit 0.
        Ok(_) => match check(provider) {
            Ok(Verdict::Ready { source }) => {
                println!("rpi: {provider} credentials are ready ({source}).");
                println!("You can now start rpi in your ACP client.");
                0
            }
            Ok(Verdict::Missing) => {
                diagln!(
                    "error: no usable credential for {provider} after login. \
                     Retry with a fresh key, or configure a provider in models.json."
                );
                1
            }
            Err(error) => {
                diagln!("error: could not verify {provider} credentials: {error}");
                1
            }
        },
        Err(error) => {
            diagln!("error: {error}");
            1
        }
    }
}

/// Provider selection shared by terminal setup, request auth, and local status.
pub(crate) fn configured_provider() -> Result<String, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    Ok(provider_arg(&args)?.unwrap_or_else(|| DEFAULT_PROVIDER.to_owned()))
}

/// The `--provider <id>` / `--provider=<id>` value, when given.
fn provider_arg(args: &[String]) -> Result<Option<String>, String> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if let Some(value) = arg.strip_prefix("--provider=") {
            if value.trim().is_empty() {
                return Err("--provider needs a value".into());
            }
            return Ok(Some(value.trim().to_string()));
        }
        if arg == "--provider" {
            return match args.get(index + 1) {
                Some(value) if !value.trim().is_empty() => Ok(Some(value.trim().to_string())),
                _ => Err("--provider needs a value".into()),
            };
        }
        index += 1;
    }
    Ok(None)
}

/// `rpi auth login --provider <id>`, with the terminal handed to its prompt.
///
/// Called only when stdin is a terminal: piping a key in does not satisfy the
/// prompt (it reads the console), so the interactive path is the only one that
/// can succeed.
fn login(provider: &str) -> Result<i32, String> {
    let status = std::process::Command::new(crate::session::resolve_rpi_executable())
        .arg("auth")
        .arg("login")
        .arg("--provider")
        .arg(provider)
        .status()
        .map_err(|error| format!("spawn `rpi auth login`: {error}"))?;
    Ok(status.code().unwrap_or(1))
}

/// `rpi auth logout` for the default provider — backs ACP's `logout` method.
///
/// Logs out the provider `--setup` configures by default. An OAuth provider
/// first revokes its remote token through its OAuth extension; a plain API key
/// is simply removed from `auth.json`.
pub fn logout() -> Result<(), String> {
    let status = std::process::Command::new(crate::session::resolve_rpi_executable())
        .arg("auth")
        .arg("logout")
        // ACP owns stdout/stdin. A logout child must never inherit either
        // descriptor: provider diagnostics on stdout would corrupt JSON-RPC,
        // and an inherited stdin could consume the client's protocol stream.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|error| format!("spawn `rpi auth logout`: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`rpi auth logout` exited {}",
            status.code().unwrap_or(1)
        ))
    }
}

/// Async logout used by the ACP dispatcher. `kill_on_drop` ensures a
/// disconnect cannot leave a credential helper running after its request task
/// is cancelled.
pub async fn logout_async() -> Result<(), String> {
    let provider = configured_provider()?;
    let status = tokio::time::timeout(CHECK_TIMEOUT,
        tokio::process::Command::new(crate::session::resolve_rpi_executable())
        .args(["auth", "logout", "--provider", &provider])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        // Provider callbacks can include credential material in diagnostics.
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status())
        .await
        .map_err(|_| "credential logout timed out".to_owned())?
        .map_err(|_| "credential logout failed".to_owned())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`rpi auth logout` exited {}",
            status.code().unwrap_or(1)
        ))
    }
}

/// What `rpi auth check` reported about the provider.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ready { source: String },
    Missing,
}

/// Credential state reported by `rpi auth check --provider <id> --json`.
pub(crate) fn check(provider: &str) -> Result<Verdict, String> {
    let output = std::process::Command::new(crate::session::resolve_rpi_executable())
        .arg("auth")
        .arg("check")
        .arg("--provider")
        .arg(provider)
        .arg("--json")
        .output()
        .map_err(|error| format!("spawn `rpi auth check`: {error}"))?;
    checked_verdict(provider, output.status, &output.stdout)
}

/// Credential checks are local-only and should complete quickly. Bound both
/// their running time and captured output; neither is controlled by the peer.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const CHECK_STDOUT_LIMIT: usize = 64 * 1024;
const CHECK_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Own the helper directly in the request future, not in a detached task.
/// Dropping this future (cancellation/disconnect) drops the kill-on-drop child.
async fn check_command(
    mut command: tokio::process::Command,
    provider: &str,
    deadline: std::time::Duration,
) -> Result<Verdict, String> {
    use tokio::io::AsyncReadExt;

    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn `rpi auth check`: {error}"))?;
    let mut stdout = child.stdout.take().expect("piped auth stdout");
    let result = tokio::time::timeout(deadline, async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take((CHECK_STDOUT_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| format!("read `rpi auth check`: {error}"))?;
        if bytes.len() > CHECK_STDOUT_LIMIT {
            return Err("`rpi auth check` stdout exceeded 65536 bytes".to_string());
        }
        let status = child
            .wait()
            .await
            .map_err(|error| format!("wait `rpi auth check`: {error}"))?;
        checked_verdict(provider, status, &bytes)
    })
    .await
    .unwrap_or_else(|_| Err("`rpi auth check` timed out".to_string()));
    // On timeout, read failure or overflow, explicitly kill and reap. The
    // fallback drop still kills if cancellation interrupts this bounded cleanup.
    if result.is_err() {
        let _ = child.start_kill();
        drop(stdout);
        let _ = tokio::time::timeout(CHECK_CLEANUP_TIMEOUT, child.wait()).await;
    }
    result
}

fn checked_verdict(
    provider: &str,
    status: std::process::ExitStatus,
    stdout: &[u8],
) -> Result<Verdict, String> {
    let stdout =
        std::str::from_utf8(stdout).map_err(|error| format!("unreadable auth stdout: {error}"))?;
    let value = status_object(stdout)?;
    if value["provider"].as_str() != Some(provider) {
        return Err("credential status provider does not match requested provider".into());
    }
    let verdict = verdict(stdout)?;
    if !status.success() && verdict != Verdict::Missing {
        return Err(format!(
            "`rpi auth check --provider {provider}` returned a ready state with exit status {}",
            status.code().unwrap_or(1)
        ));
    }
    Ok(verdict)
}

pub(crate) async fn check_async(provider: &str) -> Result<Verdict, String> {
    let mut command = tokio::process::Command::new(crate::session::resolve_rpi_executable());
    command.args(["auth", "check", "--provider", provider, "--json"]);
    check_command(command, provider, CHECK_TIMEOUT).await
}

/// Parse the `{ready, provider, source}` object `rpi auth check --json` emits.
pub(crate) fn verdict(stdout: &str) -> Result<Verdict, String> {
    let value = status_object(stdout)?;
    if value["ready"].as_bool() == Some(true) {
        let source = value["source"]
            .as_str()
            .filter(|source| !source.is_empty())
            .unwrap_or("configured");
        return Ok(Verdict::Ready {
            source: source.to_string(),
        });
    }
    if value["ready"].as_bool() == Some(false) {
        Ok(Verdict::Missing)
    } else {
        Err("credential status must contain a boolean ready field".into())
    }
}

fn status_object(stdout: &str) -> Result<Value, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| "no status object on stdout".to_string())?;
    serde_json::from_str(line.trim()).map_err(|error| format!("unreadable status object: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subprocess fixture: all configuration is child-local, never set_env.
    #[test]
    fn helper_fixture() {
        let Ok(mode) = std::env::var("RPI_AUTH_UNIT_FIXTURE") else {
            return;
        };
        let _connection = std::env::var("RPI_AUTH_UNIT_ADDRESS")
            .ok()
            .map(|address| std::net::TcpStream::connect(address).unwrap());
        match mode.as_str() {
            "hang" => std::thread::sleep(std::time::Duration::from_secs(60)),
            "overflow" => {
                use std::io::Write;
                std::io::stdout()
                    .write_all(&vec![b'x'; CHECK_STDOUT_LIMIT + 1])
                    .unwrap();
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
            "malformed" => println!("{{broken"),
            "invalid_utf8" => {
                use std::io::Write;
                std::io::stdout().write_all(&[255]).unwrap();
            }
            "wrong_provider" => println!(r#"{{"ready":true,"provider":"openai"}}"#),
            "invalid_status" => println!(r#"{{"provider":"anthropic"}}"#),
            "missing" => {
                println!(r#"{{"ready":false,"provider":"anthropic"}}"#);
                std::process::exit(1);
            }
            "failed_ready" => {
                println!(r#"{{"ready":true,"provider":"anthropic"}}"#);
                std::process::exit(2);
            }
            "ready" => println!(r#"{{"ready":true,"provider":"anthropic","source":"fixture"}}"#),
            _ => panic!("unknown fixture"),
        }
    }

    fn fixture(mode: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "auth::tests::helper_fixture", "--nocapture"]);
        command.env("RPI_AUTH_UNIT_FIXTURE", mode);
        command
    }

    #[tokio::test]
    async fn async_check_validates_status_errors_and_provider() {
        let deadline = std::time::Duration::from_secs(5);
        assert_eq!(
            check_command(fixture("ready"), DEFAULT_PROVIDER, deadline)
                .await
                .unwrap(),
            Verdict::Ready {
                source: "fixture".into()
            }
        );
        assert_eq!(
            check_command(fixture("missing"), DEFAULT_PROVIDER, deadline)
                .await
                .unwrap(),
            Verdict::Missing
        );
        for (mode, message) in [
            ("failed_ready", "exit status 2"),
            ("wrong_provider", "provider does not match"),
            ("invalid_status", "boolean ready"),
            ("malformed", "unreadable status"),
            ("invalid_utf8", "unreadable auth stdout"),
            ("overflow", "stdout exceeded"),
        ] {
            let error = check_command(fixture(mode), DEFAULT_PROVIDER, deadline)
                .await
                .unwrap_err();
            assert!(error.contains(message), "{mode}: {error}");
        }
        let error = check_command(
            tokio::process::Command::new(
                std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()),
            ),
            DEFAULT_PROVIDER,
            deadline,
        )
        .await
        .unwrap_err();
        assert!(error.contains("spawn `rpi auth check`"), "{error}");
    }

    async fn assert_connection_closed(mut stream: tokio::net::TcpStream) {
        use tokio::io::AsyncReadExt;
        let mut byte = [0];
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut byte))
                .await
                .expect("helper survived cancellation/timeout");
        // Windows may reset the connection when terminating the process.
        assert!(matches!(result, Ok(0) | Err(_)), "unexpected fixture data");
    }

    #[tokio::test]
    async fn async_check_cancellation_kills_helper() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut command = fixture("hang");
        command.env(
            "RPI_AUTH_UNIT_ADDRESS",
            listener.local_addr().unwrap().to_string(),
        );
        let task = tokio::spawn(check_command(command, DEFAULT_PROVIDER, CHECK_TIMEOUT));
        let (stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_connection_closed(stream).await;
    }

    #[tokio::test]
    async fn async_check_deadline_and_overflow_kill_and_reap_helper() {
        for mode in ["hang", "overflow"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut command = fixture(mode);
            command.env(
                "RPI_AUTH_UNIT_ADDRESS",
                listener.local_addr().unwrap().to_string(),
            );
            let task = tokio::spawn(check_command(
                command,
                DEFAULT_PROVIDER,
                std::time::Duration::from_secs(1),
            ));
            let (stream, _) =
                tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let error = task.await.unwrap().unwrap_err();
            assert!(
                error.contains(if mode == "hang" {
                    "timed out"
                } else {
                    "stdout exceeded"
                }),
                "{error}"
            );
            assert_connection_closed(stream).await;
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn setup_is_recognized_anywhere_in_the_argument_list() {
        assert!(requested(&args(&["--setup"])));
        assert!(requested(&args(&["setup"])));
        assert!(requested(&args(&["--provider", "openai", "--setup"])));
        // The ACP server invocation must never be mistaken for setup.
        assert!(!requested(&args(&[])));
        assert!(!requested(&args(&["--acp"])));
        assert!(!requested(&args(&["--install-zed"])));
    }

    #[test]
    fn provider_defaults_and_parses_both_forms() {
        assert_eq!(provider_arg(&args(&["--setup"])).unwrap(), None);
        assert_eq!(
            provider_arg(&args(&["--setup", "--provider", "openai"]))
                .unwrap()
                .as_deref(),
            Some("openai")
        );
        assert_eq!(
            provider_arg(&args(&["--provider=openai", "--setup"]))
                .unwrap()
                .as_deref(),
            Some("openai")
        );
        assert!(provider_arg(&args(&["--setup", "--provider"])).is_err());
        assert!(provider_arg(&args(&["--provider=", "--setup"])).is_err());
    }

    #[test]
    fn verdict_reads_ready_and_missing_status() {
        assert_eq!(
            verdict(r#"{"ready":true,"provider":"anthropic","source":"auth.json"}"#).unwrap(),
            Verdict::Ready {
                source: "auth.json".to_string()
            }
        );
        // A provider whose key lives in models.json is ready with that source.
        assert_eq!(
            verdict(r#"{"ready":true,"provider":"x","source":"models.json"}"#).unwrap(),
            Verdict::Ready {
                source: "models.json".to_string()
            }
        );
        assert_eq!(
            verdict(r#"{"ready":false,"provider":"anthropic","source":"none"}"#).unwrap(),
            Verdict::Missing
        );
        // Ready without a named source still counts, with a generic label.
        assert_eq!(
            verdict(r#"{"ready":true}"#).unwrap(),
            Verdict::Ready {
                source: "configured".to_string()
            }
        );
    }

    #[test]
    fn the_setup_key_reader_takes_the_first_non_empty_line() {
        // `setup` reads no piped input: `rpi auth login` prompts through the
        // terminal only, and the non-interactive path is a fast failure. Nothing
        // to unit-test beyond the provider/verdict parsing above.
    }

    #[test]
    fn verdict_tolerates_noise_and_rejects_garbage() {
        // Progress lines may precede the object; the last JSON line wins.
        assert_eq!(
            verdict("checking...\n{\"ready\":true,\"source\":\"auth.json\"}\n").unwrap(),
            Verdict::Ready {
                source: "auth.json".to_string()
            }
        );
        assert!(verdict("").is_err());
        assert!(verdict("ready: yes").is_err());
        assert!(verdict("{not json").is_err());
    }
}
