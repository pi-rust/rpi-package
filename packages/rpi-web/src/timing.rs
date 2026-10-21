//! Bridge-owned execution timings, kept beside the session registry.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, time::Instant};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// Replay anchors must describe elapsed time at delivery, not at original emission.
pub(crate) fn refresh_event(event: &mut Value) {
    if let Some(timing) = event.get_mut("webTiming") {
        if timing["running"] == true {
            let now = now_ms();
            let observed = timing["observedAt"].as_u64().unwrap_or(now);
            timing["elapsedMs"] = json!(timing["elapsedMs"]
                .as_u64()
                .unwrap_or_default()
                .saturating_add(now.saturating_sub(observed)));
            timing["observedAt"] = json!(now);
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Timing {
    elapsed_ms: u64,
}

pub(crate) struct ToolTimings {
    path: PathBuf,
    finished: HashMap<String, Timing>,
    active: HashMap<String, Instant>,
}

impl ToolTimings {
    pub fn set_session(&mut self, id: &str) {
        // Forking hosts choose their own durable id. Only use native UUIDs as filenames.
        if uuid::Uuid::parse_str(id).is_ok() {
            self.path.set_file_name(format!("{id}.json"));
        }
    }

    pub fn finish_active(&mut self) {
        if self.active.is_empty() {
            return;
        }
        for (id, start) in self.active.drain() {
            self.finished.insert(
                id,
                Timing {
                    elapsed_ms: start.elapsed().as_millis() as u64,
                },
            );
        }
        if let Err(error) = self.save() {
            eprintln!("rpi-web: cannot save interrupted tool timings: {error}");
        }
    }
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let finished = match std::fs::read(&path) {
            Ok(raw) => {
                serde_json::from_slice(&raw).map_err(|e| format!("invalid tool timings: {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(format!("cannot read tool timings: {e}")),
        };
        Ok(Self {
            path,
            finished,
            active: HashMap::new(),
        })
    }

    fn value(&self, id: &str) -> Option<Value> {
        if let Some(start) = self.active.get(id) {
            Some(json!({"elapsedMs": start.elapsed().as_millis() as u64, "running": true}))
        } else {
            self.finished
                .get(id)
                .map(|timing| json!({"elapsedMs": timing.elapsed_ms, "running": false}))
        }
    }

    pub fn event(&mut self, event: &mut Value) {
        let id = event["toolCallId"].as_str().unwrap_or_default().to_owned();
        match event["type"].as_str() {
            Some("tool_execution_start") if !id.is_empty() => {
                self.finished.remove(&id);
                self.active.entry(id.clone()).or_insert_with(Instant::now);
            }
            Some("tool_execution_end") if !id.is_empty() => {
                if let Some(start) = self.active.remove(&id) {
                    self.finished.insert(
                        id.clone(),
                        Timing {
                            elapsed_ms: start.elapsed().as_millis() as u64,
                        },
                    );
                    if let Err(error) = self.save() {
                        eprintln!("rpi-web: cannot save tool timings: {error}");
                    }
                }
            }
            _ => {}
        }
        if let Some(timing) = self.value(&id) {
            event["webTiming"] = timing;
            if event["webTiming"]["running"] == true {
                event["webTiming"]["observedAt"] = json!(now_ms());
            }
        }
        if let Some(message) = event.get_mut("message") {
            self.message(message);
        }
    }

    pub fn message(&self, message: &mut Value) {
        if message["role"] == "toolResult" {
            if let Some(timing) = message["toolCallId"].as_str().and_then(|id| self.value(id)) {
                message["webTiming"] = timing;
            }
        } else if message["role"] == "assistant" {
            if let Some(blocks) = message["content"].as_array_mut() {
                for block in blocks {
                    if block["type"] == "toolCall" {
                        if let Some(timing) = block["id"].as_str().and_then(|id| self.value(id)) {
                            block["webTiming"] = timing;
                        }
                    }
                }
            }
        }
    }

    pub fn response(&mut self, response: &mut Value) {
        if response["success"] == true && response["command"] == "get_state" {
            if let Some(id) = response["data"]["sessionId"].as_str() {
                self.set_session(id);
            }
        }
        if let Some(messages) = response
            .get_mut("data")
            .and_then(|data| data.get_mut("messages"))
            .and_then(Value::as_array_mut)
        {
            for message in messages {
                self.message(message);
            }
        }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(
            &temp,
            serde_json::to_vec(&self.finished).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(temp, &self.path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_refreshes_running_anchors_and_leaves_completed_timings_unchanged() {
        let mut running = json!({"webTiming":{"elapsedMs":12,"running":true,"observedAt":now_ms().saturating_sub(7000)}});
        refresh_event(&mut running);
        assert!(running["webTiming"]["elapsedMs"].as_u64().unwrap() >= 7012);
        let mut done = json!({"webTiming":{"elapsedMs":12,"running":false}});
        let expected = done.clone();
        refresh_event(&mut done);
        assert_eq!(done, expected);
    }

    #[test]
    fn forked_timings_use_the_native_session_id_without_mutating_other_responses() {
        let dir = std::env::temp_dir().join(format!("rpi-timings-{}", uuid::Uuid::new_v4()));
        let mut timings = ToolTimings::load(dir.join("source.json")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        timings
            .response(&mut json!({"command":"get_state","success":true,"data":{"sessionId":id}}));
        let mut response = json!({"command":"get_queue","success":true});
        let expected = response.clone();
        timings.response(&mut response);
        assert_eq!(response, expected);
        timings.event(&mut json!({"type":"tool_execution_start","toolCallId":"interrupted"}));
        timings.finish_active();
        let restored = ToolTimings::load(dir.join(format!("{id}.json"))).unwrap();
        assert_eq!(restored.value("interrupted").unwrap()["running"], false);
        assert!(!dir.join("source.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timings_survive_reload_and_do_not_invent_old_durations() {
        let dir = std::env::temp_dir().join(format!("rpi-timings-{}", uuid::Uuid::new_v4()));
        let path = dir.join("session.json");
        let mut timings = ToolTimings::load(path.clone()).unwrap();
        let mut start = json!({"type":"tool_execution_start","toolCallId":"bash-1"});
        timings.event(&mut start);
        assert_eq!(start["webTiming"]["running"], true);
        timings.active.insert(
            "bash-1".into(),
            Instant::now() - std::time::Duration::from_millis(1234),
        );
        let mut end = json!({"type":"tool_execution_end","toolCallId":"bash-1","isError":true});
        timings.event(&mut end);
        assert!(end["webTiming"]["elapsedMs"].as_u64().unwrap() >= 1234);
        assert_eq!(end["webTiming"]["running"], false);
        // A second completed tool also checks replacement of an existing file on Windows.
        timings.event(&mut json!({"type":"tool_execution_start","toolCallId":"bash-2"}));
        timings.event(&mut json!({"type":"tool_execution_end","toolCallId":"bash-2"}));
        let mut restored = ToolTimings::load(path).unwrap();
        let mut result = json!({"data":{"messages":[{"role":"toolResult","toolCallId":"bash-1"},{"role":"toolResult","toolCallId":"old"}]}});
        restored.response(&mut result);
        assert_eq!(result["data"]["messages"][0]["webTiming"], end["webTiming"]);
        assert!(result["data"]["messages"][1].get("webTiming").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
