//! Durable native rpi sessions and ACP replay of the backend's active branch.
use crate::session::translate_event;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub(crate) fn session_directory(id: &str) -> Result<PathBuf, String> {
    if uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.to_string())
        .as_deref()
        != Ok(id)
    {
        return Err("unknown sessionId: older ACP versions used temporary sessions that cannot be restored after exit".into());
    }
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
    Ok(root.join(id))
}

// rpi stores JSONL files in a cwd-named subdirectory of --session-dir.
pub(crate) fn saved_session(id: &str) -> Result<PathBuf, String> {
    find_saved_session(&session_directory(id)?, id)
}

fn find_saved_session(directory: &Path, id: &str) -> Result<PathBuf, String> {
    let mut found = None;
    let directories = std::fs::read_dir(directory)
        .map_err(|_| "unknown sessionId: no saved ACP session".to_string())?;
    for entry in directories {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(entry.path()).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            if !file.file_type().map_err(|e| e.to_string())?.is_file()
                || !file
                    .file_name()
                    .to_string_lossy()
                    .ends_with(&format!("_{id}.jsonl"))
            {
                continue;
            }
            let path = file.path();
            let mut reader = BufReader::new(std::fs::File::open(&path).map_err(|e| e.to_string())?);
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            let header: Value =
                serde_json::from_str(&line).map_err(|e| format!("invalid session header: {e}"))?;
            if header["kind"] != "header" || header["version"] != 4 || header["id"] != id {
                return Err("invalid saved session header".into());
            }
            if found.replace(path).is_some() {
                return Err("multiple saved files for sessionId".into());
            }
        }
    }
    found.ok_or_else(|| "unknown sessionId: no saved ACP session".into())
}

pub(crate) fn replay(path: &Path, leaf: Option<&str>) -> Result<Vec<Value>, String> {
    let reader = BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
    let mut entries = HashMap::new();
    for line in reader.lines().skip(1) {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: Value =
            serde_json::from_str(&line).map_err(|e| format!("invalid session history: {e}"))?;
        if entry["kind"] == "entry" {
            let id = entry["id"]
                .as_str()
                .ok_or("history entry has no id")?
                .to_owned();
            if entries.insert(id, entry).is_some() {
                return Err("duplicate history entry id".into());
            }
        }
    }
    branch_updates(&entries, leaf)
}

fn branch_updates<'a>(
    entries: &'a HashMap<String, Value>,
    mut leaf: Option<&'a str>,
) -> Result<Vec<Value>, String> {
    let mut path = Vec::new();
    let mut visited = HashSet::new();
    while let Some(id) = leaf {
        if !visited.insert(id) {
            return Err("cycle in saved session history".into());
        }
        let entry = entries
            .get(id)
            .ok_or("saved session history is missing a branch entry")?;
        path.push(entry);
        leaf = entry["parentId"].as_str();
    }
    let mut updates = Vec::new();
    for entry in path.into_iter().rev() {
        if entry["type"] == "message" {
            updates.extend(message_updates(&entry["message"]));
        }
    }
    Ok(updates)
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
