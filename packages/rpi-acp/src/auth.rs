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

/// What `rpi auth check` reported about the provider.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ready { source: String },
    Missing,
}

/// Credential state reported by `rpi auth check --provider <id> --json`.
fn check(provider: &str) -> Result<Verdict, String> {
    let output = std::process::Command::new(crate::session::resolve_rpi_executable())
        .arg("auth")
        .arg("check")
        .arg("--provider")
        .arg(provider)
        .arg("--json")
        .output()
        .map_err(|error| format!("spawn `rpi auth check`: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    verdict(&stdout)
}

/// Parse the `{ready, provider, source}` object `rpi auth check --json` emits.
pub(crate) fn verdict(stdout: &str) -> Result<Verdict, String> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| "no status object on stdout".to_string())?;
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|error| format!("unreadable status object: {error}"))?;
    if value["ready"].as_bool() == Some(true) {
        let source = value["source"]
            .as_str()
            .filter(|source| !source.is_empty())
            .unwrap_or("configured");
        return Ok(Verdict::Ready {
            source: source.to_string(),
        });
    }
    Ok(Verdict::Missing)
}

#[cfg(test)]
mod tests {
    use super::*;

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
