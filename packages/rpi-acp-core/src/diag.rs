//! Diagnostics that survive a client that swallows stderr.
//!
//! Zed forwards an agent's stderr into its own log only some of the time — a
//! failing connection can leave nothing behind but the client's own "transport
//! closed". When the [`RPI_ACP_LOG`] environment variable names a file (or
//! [`RPI_ACP_DEBUG`] is on, which defaults to `rpi-acp.log` in the temp
//! directory) every diagnostic is appended there as well as to stderr, so the
//! agent's own account of what happened is always recoverable.

use std::io::Write;

/// Write one diagnostic line to stderr and, when configured, to the log file.
///
/// Used as `diagln!("...{value}")`; import it with `use crate::diagln;`.
#[macro_export]
macro_rules! diagln {
    ($($arg:tt)*) => {
        $crate::diag::log(format_args!($($arg)*))
    };
}

/// Mirror diagnostics into `RPI_ACP_LOG`, or the default file under debug.
///
/// Three ways to switch it on, cheapest first for whoever is debugging:
///
/// 1. `RPI_ACP_LOG=<path>` — explicit destination.
/// 2. a marker file `<temp>/rpi-acp-debug` — no environment plumbing at all, so
///    it also works when something else spawns the agent and does not pass env
///    (a GUI editor launching through a shell, say).
/// 3. `RPI_ACP_DEBUG=1` — implies the default file.
pub fn log_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("RPI_ACP_LOG") {
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    let temp = std::env::temp_dir();
    if temp.join(MARKER).is_file() || debug_enabled() {
        return Some(temp.join("rpi-acp.log"));
    }
    None
}

/// Drop this file in the temp directory to turn on file logging.
pub const MARKER: &str = "rpi-acp-debug";

/// `RPI_ACP_DEBUG=1` traces the method stream and implies file logging.
pub fn debug_enabled() -> bool {
    flag("RPI_ACP_DEBUG")
}

/// Write one diagnostic line to stderr and, when configured, to the log file.
///
/// The file is opened per line: these are a handful of lines per session, and
/// that keeps the hot path free of shared state or locking.
pub fn log(args: std::fmt::Arguments<'_>) {
    let line = args.to_string();
    eprintln!("{line}");
    if let Some(path) = log_path() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// Treat unset/empty/`0`/`false` as off, anything else as on.
fn flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_path_follows_the_environment() {
        // Whatever the ambient environment says, an explicit path always wins.
        std::env::set_var("RPI_ACP_LOG", "/tmp/rpi-acp-test.log");
        assert_eq!(
            log_path(),
            Some(std::path::PathBuf::from("/tmp/rpi-acp-test.log"))
        );
        std::env::remove_var("RPI_ACP_LOG");

        std::env::remove_var("RPI_ACP_DEBUG");
        assert!(log_path().is_none());
        std::env::set_var("RPI_ACP_DEBUG", "1");
        assert!(log_path().is_some_and(|path| path.ends_with("rpi-acp.log")));
        std::env::remove_var("RPI_ACP_DEBUG");
    }

    #[test]
    fn flag_truthiness() {
        std::env::remove_var("RPI_ACP_DIAG_TEST");
        assert!(!flag("RPI_ACP_DIAG_TEST"));
        for on in ["1", "true", "yes"] {
            std::env::set_var("RPI_ACP_DIAG_TEST", on);
            assert!(flag("RPI_ACP_DIAG_TEST"), "{on}");
        }
        for off in ["", "0", "false", "FALSE"] {
            std::env::set_var("RPI_ACP_DIAG_TEST", off);
            assert!(!flag("RPI_ACP_DIAG_TEST"), "{off:?}");
        }
        std::env::remove_var("RPI_ACP_DIAG_TEST");
    }
}
