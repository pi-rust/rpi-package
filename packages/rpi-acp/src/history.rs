//! Durable native rpi sessions and ACP replay of the backend's active branch.
use crate::session::translate_event;
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

pub(crate) fn session_directory(id: &str) -> Result<PathBuf, String> {
    if uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.to_string())
        .as_deref()
        != Ok(id)
    {
        return Err("unknown sessionId: older ACP versions used temporary sessions that cannot be restored after exit; start a new conversation".into());
    }
    let root = session_root()?;
    Ok(root.join(id))
}

const ADDITIONAL_DIRECTORIES_FILE: &str = "acp-additional-directories.json";

pub(crate) fn save_additional_directories(id: &str, directories: &[PathBuf]) -> Result<(), String> {
    let directory = session_directory(id)?;
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create ACP session metadata directory: {error}"))?;
    let path = directory.join(ADDITIONAL_DIRECTORIES_FILE);
    let values = directories
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec_pretty(&values)
        .map_err(|error| format!("could not encode ACP workspace roots: {error}"))?;
    std::fs::write(path, bytes)
        .map_err(|error| format!("could not persist ACP workspace roots: {error}"))
}

pub(crate) fn load_additional_directories(id: &str) -> Result<Vec<PathBuf>, String> {
    let path = session_directory(id)?.join(ADDITIONAL_DIRECTORIES_FILE);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("could not read ACP workspace roots: {error}")),
    };
    let values: Vec<String> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid ACP workspace roots metadata: {error}"))?;
    let mut directories = Vec::with_capacity(values.len());
    for value in values {
        let path = PathBuf::from(&value);
        if !path.is_absolute() || !path.is_dir() {
            return Err(format!(
                "persisted ACP workspace root is no longer an existing absolute directory: {value}"
            ));
        }
        let path = std::fs::canonicalize(&path).map_err(|error| {
            format!("could not resolve persisted ACP workspace root {value}: {error}")
        })?;
        if !directories.contains(&path) {
            directories.push(path);
        }
    }
    Ok(directories)
}

// rpi stores JSONL files in a cwd-named subdirectory of --session-dir.
pub(crate) fn saved_session(id: &str) -> Result<PathBuf, String> {
    find_saved_session(&session_directory(id)?, id)
}

fn find_saved_session(directory: &Path, id: &str) -> Result<PathBuf, String> {
    rpi_extcore::history::find_saved_session(directory, id)
        .map_err(|error| error.replace("no saved native session", "no saved ACP session"))
}
pub(crate) async fn replay(
    session: &crate::session::Session,
) -> Result<(rpi_extcore::history::page::HistoryRead, rpi_extcore::history::page::HistorySpool), String> {
    let read = session.history_read().await?;
    let spool = rpi_extcore::history::page::spool(&read).await?;
    Ok((read, spool))
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

pub(crate) fn message_updates(message: &Value) -> Vec<Value> {
    let role = message["role"].as_str().unwrap_or("");
    if role == "toolResult" {
        return translate_event(
            &json!({"type":"tool_execution_end", "toolCallId":message["toolCallId"],
            "isError":message["isError"], "result":message}),
        )
        .into_iter()
        .collect();
    }
    let message_id = history_message_id(message);
    let thought_id = format!("{message_id}:thought");
    let kind = match role {
        "user" => "user_message_chunk",
        "assistant" => "agent_message_chunk",
        _ => return Vec::new(),
    };
    let mut updates = Vec::new();
    if let Some(text) = message["content"].as_str() {
        updates.push(json!({"sessionUpdate":kind,"content":{"type":"text","text":text},"messageId":message_id}));
    } else if let Some(blocks) = message["content"].as_array() {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => updates.push(json!({"sessionUpdate":kind,"content":{"type":"text","text":block["text"]},"messageId":message_id})),
                Some(media @ ("image" | "audio")) => {
                    if let (Some(data), Some(mime_type)) = (block["data"].as_str(), block["mimeType"].as_str()) {
                        if !data.is_empty() && !mime_type.is_empty() {
                            updates.push(json!({"sessionUpdate":kind,"content":{
                                "type":media,"data":data,"mimeType":mime_type
                            },"messageId":message_id}));
                        }
                    }
                }
                Some("thinking") if role == "assistant" => updates.push(json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":block["thinking"]},"messageId":thought_id})),
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

fn history_message_id(message: &Value) -> String {
    if message["role"] == "assistant" {
        if let Some(message_id) = message["messageId"].as_str().filter(|id| !id.is_empty()) {
            return message_id.to_owned();
        }
        if let Some(response_id) = message["responseId"].as_str().filter(|id| !id.is_empty()) {
            return format!("assistant:{response_id}");
        }
        if let Some(timestamp) = message["timestamp"].as_i64() {
            return format!("assistant:{timestamp}");
        }
    }
    if let Some(id) = message["id"].as_str().filter(|id| !id.is_empty()) {
        return id.to_owned();
    }
    if let Some(id) = message["entryId"].as_str().filter(|id| !id.is_empty()) {
        return format!("history:{id}");
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(message.to_string().as_bytes());
    format!("history:{}", hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_preserves_media_order_bytes_and_message_identity() {
        let message = json!({"role":"user", "entryId":"media-entry", "content":[
            {"type":"text", "text":"before"},
            {"type":"image", "data":"aW1hZ2U=", "mimeType":"image/png"},
            {"type":"text", "text":"between"},
            {"type":"audio", "data":"YXVkaW8=", "mimeType":"audio/wav"},
            {"type":"text", "text":"after"}
        ]});
        let updates = message_updates(&message);
        assert_eq!(updates.len(), 5);
        for (update, content) in updates.iter().zip(message["content"].as_array().unwrap()) {
            assert_eq!(update["sessionUpdate"], "user_message_chunk");
            assert_eq!(update["content"], *content);
            assert_eq!(update["messageId"], "history:media-entry");
            let mapped = crate::v2::map_session_update(update.clone());
            assert_eq!(mapped["content"], *content);
        }
        let image_only = json!({"role":"assistant", "messageId":"image-response", "content":[
            {"type":"image", "data":"aW1hZ2U=", "mimeType":"image/png"}
        ]});
        assert_eq!(message_updates(&image_only)[0]["messageId"], "image-response");
    }

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
    fn legacy_message_ids_are_stable_across_reloads() {
        let message = json!({"role":"assistant","content":"legacy"});
        assert_eq!(
            history_message_id(&message),
            "history:acee9369f80b262826d6c35fb381f5407141f4a7b008c960808df1bab1e67171"
        );
        assert_eq!(history_message_id(&message), history_message_id(&message));
    }

    #[test]
    fn native_entry_ids_keep_identical_messages_distinct() {
        let first = json!({"entryId":"a","role":"user","content":"same"});
        let second = json!({"entryId":"b","role":"user","content":"same"});
        assert_ne!(message_updates(&first)[0]["messageId"], message_updates(&second)[0]["messageId"]);
        assert_eq!(history_message_id(&first), "history:a");
        let assistant = json!({"entryId":"a","role":"assistant","messageId":"live-id","content":"text"});
        assert_eq!(history_message_id(&assistant), "live-id");
    }

    #[test]
    fn cursors_reject_wrong_session_and_impossible_bounds() {
        let valid = json!({"version":1,"session_id":"s","instance_id":"0189e82d-9328-4f26-9dce-749a3350e710","anchor":{"leaf_id":"leaf","total":3},"before":"entry","boundary":7});
        assert!(validate_cursor(&valid.to_string(), "s").is_ok());
        assert!(validate_cursor(&valid.to_string(), "other").is_err());
        for (key, value) in [("version", json!(2)), ("before", json!("")), ("boundary", json!(-1)), ("extra", json!(true))] {
            let mut bad = valid.clone();
            bad[key] = value;
            assert!(validate_cursor(&bad.to_string(), "s").is_err());
        }
        for value in [json!({"leaf_id":"","total":3}), json!({"leaf_id":"leaf","total":1})] {
            let mut bad = valid.clone();
            bad["anchor"] = value;
            assert!(validate_cursor(&bad.to_string(), "s").is_err());
        }
        assert!(validate_cursor(&"x".repeat(8193), "s").is_err());
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

/// Private `_rpi/history` cursor, not a standard ACP replay cursor. Bound to
/// the native branch, session and atomic event boundary; no stable ACP fields
/// or message IDs are invented. A changed branch/boundary requires restarting.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryCursor {
    version: u8,
    session_id: String,
    instance_id: String,
    anchor: rpi_extcore::history::page::BranchAnchor,
    before: String,
    boundary: u64,
}

pub(crate) fn validate_cursor(value: &str, session_id: &str) -> Result<HistoryCursor, String> {
    if value.len() > 8192 { return Err("invalid history cursor".into()); }
    let cursor: HistoryCursor = serde_json::from_str(value).map_err(|_| "invalid history cursor")?;
    if cursor.version != 1 || cursor.session_id != session_id || cursor.before.is_empty()
        || uuid::Uuid::parse_str(&cursor.instance_id).is_err()
        || cursor.before.len() > 1024 || cursor.anchor.total < 2
        || cursor.anchor.leaf_id.as_ref().is_some_and(|id| id.is_empty() || id.len() > 1024)
    { return Err("invalid history cursor".into()); }
    Ok(cursor)
}

pub(crate) async fn page(
    session: &crate::session::Session,
    session_id: &str,
    cursor: Option<HistoryCursor>,
    limit: usize,
    v2: bool,
) -> Result<Value, String> {
    let read = session.history_read().await?;
    // Subscription takes subscribe + boundary atomically in core. Do not sample
    // snapshot/status separately and label it as the transcript's boundary.
    let boundary = read.subscription.replay.last().map_or(0, |event| event.seq);
    if cursor.as_ref().is_some_and(|cursor| cursor.boundary != boundary || cursor.instance_id != read.instance_id.to_string()) {
        return Err("history attachment or event boundary changed; restart history read".into());
    }
    let page = read.page(cursor.as_ref().map(|c| c.before.as_str()), cursor.as_ref().map(|c| &c.anchor), limit).await?;
    read.verify(&page.anchor).await?;
    let updates = page.messages.iter().flat_map(message_updates).map(|update| {
        if v2 { crate::v2::map_session_update(update) } else { update }
    }).collect::<Vec<_>>();
    let next = if page.has_more {
        Some(serde_json::to_string(&HistoryCursor {
            version: 1, session_id: session_id.to_owned(), instance_id: read.instance_id.to_string(), anchor: page.anchor,
            before: page.first_id.ok_or("missing history cursor")?, boundary,
        }).map_err(|e| e.to_string())?)
    } else { None };
    Ok(json!({"updates":updates,"nextCursor":next,"hasMore":page.has_more,"_meta":{"rpi/history":{"eventBoundary":boundary,"order":"oldest-first","pagination":"older"}}}))
}
