//! Durable native rpi sessions and ACP replay of the backend's active branch.
use crate::session::translate_event;
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(crate) fn session_directory(id: &str) -> Result<PathBuf, String> {
    if uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.to_string())
        .as_deref()
        != Ok(id)
    {
        return Err("unknown sessionId: older ACP versions used temporary sessions that cannot be restored after exit; start a new conversation".into());
    }
    Ok(session_root()?.join(id))
}

/// Resolve configuration only; listing captures this once per connection.
/// Keep UUID validation in session_directory before resolving the root.
pub(crate) fn session_root() -> Result<PathBuf, String> {
    let root = if let Some(path) = std::env::var_os("RPI_ACP_SESSION_DIR") {
        PathBuf::from(path)
    } else {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .ok_or("cannot locate the home directory for ACP session storage")?;
        PathBuf::from(home).join(".rpi/agent/acp/sessions")
    };
    if !root.is_absolute() {
        return Err("RPI_ACP_SESSION_DIR must be an absolute path".into());
    }
    Ok(root)
}

// rpi stores JSONL files in a cwd-named subdirectory of --session-dir.
pub(crate) fn saved_session(id: &str) -> Result<PathBuf, String> {
    find_saved_session(&session_directory(id)?, id)
}

fn find_saved_session(directory: &Path, id: &str) -> Result<PathBuf, String> {
    rpi_extcore::history::find_saved_session(directory, id)
        .map_err(|error| error.replace("no saved native session", "no saved ACP session"))
}
/// For fresh restores with MCP, the native header decides whether launch cwd
/// will be adopted. Reject conflicts rather than silently rebase client config.
/// Empty/missing/non-directory recorded cwd retains native launch-cwd fallback.
pub(crate) fn validate_restored_workspace(
    saved: &Path,
    id: &str,
    requested: &Path,
) -> Result<(), String> {
    let metadata = rpi_extcore::history::saved_session_metadata(saved, id)
        .map_err(|_| "cannot validate saved session workspace for MCP configuration")?;
    if metadata.cwd.as_os_str().is_empty() {
        return Ok(());
    }
    // Native evaluates relative recorded paths in the launched child's cwd.
    let recorded = if metadata.cwd.is_absolute() {
        metadata.cwd
    } else {
        requested.join(metadata.cwd)
    };
    if !recorded.is_dir() {
        return Ok(());
    }
    let recorded = recorded
        .canonicalize()
        .map_err(|_| "cannot resolve saved session workspace for MCP configuration")?;
    let requested = requested
        .canonicalize()
        .map_err(|_| "cannot resolve requested workspace for MCP configuration")?;
    // Canonical paths resolve directory spelling, symlinks/junctions and `..`.
    // Do not lowercase paths: Windows can also host case-sensitive directories.
    if recorded != requested {
        return Err("saved session workspace differs from requested cwd; reopen with its recorded cwd to configure MCP servers".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) static FAIL_REPLAY_EMIT_FOR: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);

pub(crate) struct Replay {
    // Keep the native run lease until replay and the load response complete.
    _read: rpi_extcore::history::page::HistoryRead,
    spool: rpi_extcore::history::page::HistorySpool,
}
impl Replay {
    pub(crate) async fn prepare(
        session: &crate::session::Session,
        id: &str,
    ) -> Result<Self, String> {
        let read = session.history_read().await?;
        let saved = if read.supports_pages() {
            None
        } else {
            Some(saved_session(id)?)
        };
        let spool = read.prepare(saved.as_deref()).await?;
        Ok(Self { _read: read, spool })
    }
    pub(crate) async fn emit(
        &mut self,
        writer: &crate::protocol::Writer,
        id: &str,
    ) -> Result<(), String> {
        // Deterministically exercise a spool-read failure after preparation,
        // without closing the still-live native session or ACP connection.
        #[cfg(test)]
        {
            let mut fault = FAIL_REPLAY_EMIT_FOR.lock().unwrap();
            if fault.as_deref() == Some(id) {
                *fault = None;
                return Err("fixture replay spool read failed".into());
            }
        }
        while let Some(messages) = self.spool.pop()? {
            self._read.ensure_active()?;
            for message in messages {
                for update in message_updates(&message) {
                    writer
                        .notify("session/update", json!({"sessionId":id,"update":update}))
                        .await;
                }
            }
        }
        Ok(())
    }
}
#[cfg(test)]
fn branch_updates(
    entries: &HashMap<String, Value>,
    leaf: Option<&str>,
) -> Result<Vec<Value>, String> {
    Ok(
        rpi_extcore::history::branch_messages_from_entries(entries, leaf)?
            .iter()
            .flat_map(message_updates)
            .collect(),
    )
}

fn message_updates(message: &Value) -> Vec<Value> {
    let role = message["role"].as_str().unwrap_or("");
    if role == "toolResult" {
        return translate_event(
            &json!({"type":"tool_execution_end", "toolCallId":message["toolCallId"],
            "isError":message["isError"], "result":message}),
        )
        .into_iter()
        .collect();
    }
    let kind = match role {
        "user" => "user_message_chunk",
        "assistant" => "agent_message_chunk",
        _ => return Vec::new(),
    };
    let mut updates = Vec::new();
    if let Some(text) = message["content"].as_str() {
        updates.push(json!({"sessionUpdate":kind,"content":{"type":"text","text":text}}));
    } else if let Some(blocks) = message["content"].as_array() {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => updates.push(json!({"sessionUpdate":kind,"content":{"type":"text","text":block["text"]}})),
                Some("thinking") if role == "assistant" => updates.push(json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":block["thinking"]}})),
                Some("toolCall") if role == "assistant" => {
                    if let Some(update) = translate_event(&json!({"type":"tool_execution_start", "toolCallId":block["id"],
                        "toolName":block["name"],"args":block["arguments"]})) { updates.push(update); }
                }
                _ => {}
            }
        }
    }
    updates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_follows_active_branch_and_preserves_tool_results() {
        let entries: HashMap<String,Value> = [
            ("u", json!({"type":"message","parentId":null,"message":{"role":"user","content":"hello"}})),
            ("discarded", json!({"type":"message","parentId":"u","message":{"role":"assistant","content":[{"type":"text","text":"wrong branch"}]}})),
            ("a", json!({"type":"message","parentId":"u","message":{"role":"assistant","content":[{"type":"thinking","thinking":"think"},{"type":"toolCall","id":"call1","name":"read","arguments":{"path":"file"}}]}})),
            ("t", json!({"type":"message","parentId":"a","message":{"role":"toolResult","toolCallId":"call1","isError":false,"content":[{"type":"text","text":"contents"}]}})),
            ("z", json!({"type":"message","parentId":"t","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}})),
        ].into_iter().map(|(id,v)|(id.into(),v)).collect();
        let updates = branch_updates(&entries, Some("z")).unwrap();
        assert_eq!(updates.len(), 5);
        assert_eq!(updates[0]["sessionUpdate"], "user_message_chunk");
        assert_eq!(updates[1]["sessionUpdate"], "agent_thought_chunk");
        assert_eq!(updates[2]["toolCallId"], "call1");
        assert_eq!(updates[3]["status"], "completed");
        assert_eq!(updates[3]["content"][0]["content"]["text"], "contents");
        assert_eq!(updates[4]["content"]["text"], "done");
    }

    #[test]
    fn invalid_branches_and_session_paths_fail() {
        assert!(session_directory("../../outside").is_err());
        assert!(session_directory("rpi-123-1").is_err());
        let entries = HashMap::from([("a".into(), json!({"parentId":"a"}))]);
        assert!(branch_updates(&entries, Some("a")).is_err());
        assert!(branch_updates(&entries, Some("missing")).is_err());
    }
}
