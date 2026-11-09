//! Background children must not allocate a console for protocol adapters without a console.
use std::ffi::OsStr;
use tokio::process::Command;

pub fn background_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: redirecting stdio alone does not prevent Windows
        // from opening a console for git.exe or the native RPC host.
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{process::Stdio, time::Duration};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn background_child_has_no_console_and_preserves_piped_stdio() {
        let script = r#"
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class ConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'
[Console]::WriteLine([ConsoleProbe]::GetConsoleWindow().ToInt64())
[Console]::WriteLine([Console]::ReadLine())
[Console]::Error.WriteLine('background stderr')
"#;
        let mut child = background_command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start Windows console probe");
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"background stdin\n").await.unwrap();
        drop(stdin);
        // Windows hosted runners can take longer to start PowerShell while the
        // complete package test suite is running in parallel. Keep this a
        // bounded probe, but do not turn runner startup contention into a
        // false lifecycle failure.
        let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
            .await
            .expect("console probe timed out")
            .expect("read console probe output");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout)
                .unwrap()
                .replace("\r\n", "\n"),
            "0\nbackground stdin\n"
        );
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            "background stderr"
        );
    }
}

/// Native permission policy selected by the caller, never inferred from a protocol.
pub fn configure_permission_gate(command: &mut Command, bypass: bool, no_auto_gate: bool) {
    if bypass {
        command.arg("--bypass").env("RPI_PERMISSION_GATE", "0");
    } else if !no_auto_gate {
        command.arg("--permission-gate");
    }
}

use std::path::{Path, PathBuf};
pub fn resolve_rpi_executable() -> PathBuf {
    if let Some(path) = env_non_empty("RPI_BIN") {
        return PathBuf::from(path);
    }
    if let Ok(current) = std::env::current_exe() {
        if is_rpi_binary(&current) {
            return current;
        }
        if let Some(sibling) = sibling_rpi(current.parent()) {
            return sibling;
        }
    }
    PathBuf::from("rpi")
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Whether an executable path looks like the `rpi` host (not `rpi-acp`).
pub fn is_rpi_binary(path: &Path) -> bool {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.eq_ignore_ascii_case("rpi"))
        .unwrap_or(false)
}

pub fn sibling_rpi(dir: Option<&Path>) -> Option<PathBuf> {
    let dir = dir?;
    let candidate = dir.join(format!("rpi{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}
