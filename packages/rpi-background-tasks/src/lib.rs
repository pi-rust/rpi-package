use std::collections::HashMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

mod kit;

use crate::kit::{optional_string, string_param};
use serde_json::Value;

struct Task {
    child: Child,
    command: String,
    log_path: PathBuf,
}

impl Drop for Task {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

static TASKS: OnceLock<Mutex<HashMap<String, Task>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn tasks() -> &'static Mutex<HashMap<String, Task>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn task_state(id: &str, task: &mut Task) -> Value {
    let (state, exit_code) = match task.child.try_wait() {
        Ok(Some(status)) => ("completed", status.code()),
        Ok(None) => ("running", None),
        Err(_) => ("unknown", None),
    };
    serde_json::json!({
        "id": id,
        "state": state,
        "exitCode": exit_code,
        "command": task.command,
        "logPath": task.log_path,
    })
}

fn start(params: &Value) -> Result<String, String> {
    let command = string_param(params, "command")?;
    if command.trim().is_empty() || command.len() > 8000 {
        return Err("command must contain 1-8000 characters".into());
    }
    let id = format!(
        "rpi-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::SeqCst)
    );
    let log_dir = std::env::temp_dir().join("rpi-background-tasks");
    fs::create_dir_all(&log_dir)
        .map_err(|err| format!("failed to create task log directory: {err}"))?;
    let log_path = log_dir.join(format!("{id}.log"));
    let stdout =
        File::create(&log_path).map_err(|err| format!("failed to create task log: {err}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|err| format!("failed to open task log: {err}"))?;

    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/D", "/S", "/C", &command]);
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &command]);
        cmd
    };
    if let Some(cwd) = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        cmd.current_dir(cwd);
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|err| format!("failed to start background task: {err}"))?;

    tasks()
        .lock()
        .map_err(|_| "background task registry is poisoned".to_string())?
        .insert(
            id.clone(),
            Task {
                child,
                command,
                log_path: log_path.clone(),
            },
        );
    Ok(serde_json::json!({"id":id,"state":"running","logPath":log_path}).to_string())
}

fn background_task(params: &Value) -> Result<String, String> {
    let action = optional_string(params, "action", "start");
    if action == "start" {
        return start(params);
    }
    let mut registry = tasks()
        .lock()
        .map_err(|_| "background task registry is poisoned".to_string())?;
    match action.as_str() {
        "status" => {
            let id = string_param(params, "id")?;
            let task = registry
                .get_mut(&id)
                .ok_or_else(|| format!("unknown background task `{id}`"))?;
            Ok(task_state(&id, task).to_string())
        }
        "list" => {
            let values: Vec<Value> = registry
                .iter_mut()
                .map(|(id, task)| task_state(id, task))
                .collect();
            Ok(Value::Array(values).to_string())
        }
        "cancel" => {
            let id = string_param(params, "id")?;
            let task = registry
                .get_mut(&id)
                .ok_or_else(|| format!("unknown background task `{id}`"))?;
            if task
                .child
                .try_wait()
                .map_err(|err| err.to_string())?
                .is_none()
            {
                task.child
                    .kill()
                    .map_err(|err| format!("failed to cancel task: {err}"))?;
                let _ = task.child.wait();
            }
            Ok(
                serde_json::json!({"id":id,"state":"cancelled","logPath":task.log_path})
                    .to_string(),
            )
        }
        _ => Err("action must be one of: start, status, list, cancel".into()),
    }
}

export_single_tool_plugin!(
    background_task,
    "background_task",
    "Start, inspect, list, or cancel a tracked background shell task.",
    r#"{"type":"object","properties":{"action":{"type":"string","enum":["start","status","list","cancel"]},"command":{"type":"string","maxLength":8000},"cwd":{"type":"string"},"id":{"type":"string"}}}"#
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_starts_empty() {
        let value = serde_json::json!({"action":"list"});
        let result: Value = serde_json::from_str(&background_task(&value).unwrap()).unwrap();
        assert!(result.is_array());
    }
}
