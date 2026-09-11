use std::process::{Command, Stdio};
use std::time::Duration;

mod kit;

use crate::kit::optional_string;
use serde_json::Value;
use wait_timeout::ChildExt;

fn code_lens(params: &Value) -> Result<String, String> {
    let check = optional_string(params, "check", "diff");
    let (program, args): (&str, &[&str]) = match check.as_str() {
        "diff" => ("git", &["diff", "--check"]),
        "cargo" => ("cargo", &["check"]),
        "fmt" => ("cargo", &["fmt", "--", "--check"]),
        "clippy" => ("cargo", &["clippy", "--", "-D", "warnings"]),
        _ => return Err("check must be one of: diff, cargo, fmt, clippy".into()),
    };
    let timeout = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(1, 120);
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("unable to run {program}: {err}"))?;
    if child
        .wait_timeout(Duration::from_secs(timeout))
        .map_err(|err| format!("failed while waiting for {check}: {err}"))?
        .is_none()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{check} timed out after {timeout}s"));
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("failed to collect {check} output: {err}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if text.len() > 40_000 {
        text.truncate(40_000);
        text.push_str("\n[truncated]");
    }
    Ok(format!(
        "check={check} exit={}\n{}",
        output.status.code().unwrap_or(-1),
        text.trim()
    ))
}

export_single_tool_plugin!(
    code_lens,
    "code_lens",
    "Run an allowlisted, timeout-bounded local code diagnostic.",
    r#"{"type":"object","properties":{"check":{"type":"string","enum":["diff","cargo","fmt","clippy"]},"cwd":{"type":"string"},"timeoutSeconds":{"type":"integer","minimum":1,"maximum":120}}}"#
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_check() {
        let value = serde_json::json!({"check":"shell"});
        assert!(code_lens(&value).unwrap_err().contains("diff"));
    }
}
