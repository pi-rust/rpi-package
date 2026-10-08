//! Persistent session selection scoped to an IM profile and chat.
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static STORE_LOCK: Mutex<()> = Mutex::new(());

fn load(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({"version":1,"chats":{}}));
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() > 1_048_576 {
        return Err("IM session store exceeds 1 MiB".into());
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid IM session store: {error}"))?;
    if value["version"] != 1 || !value["chats"].is_object() {
        return Err("invalid IM session store schema".into());
    }
    Ok(value)
}

fn key(profile: &str, chat: &str) -> String {
    format!("{profile}\n{chat}")
}

fn selection<'a>(value: &'a Value, profile: &str, chat: &str) -> Result<Option<&'a str>, String> {
    let entry = &value["chats"][key(profile, chat)];
    if entry.is_null() {
        return Ok(None);
    }
    let selected = entry["selected"]
        .as_str()
        .ok_or("invalid IM session selection")?;
    let known = entry["sessions"]
        .as_array()
        .ok_or("invalid IM session list")?;
    let base = super::conversation_session_id(chat);
    if !(selected == base || selected.starts_with(&format!("{base}-")))
        || !selected
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        || !known.iter().any(|id| id.as_str() == Some(selected))
    {
        return Err("IM session selection is not owned by this chat".into());
    }
    Ok(Some(selected))
}

pub fn selected(path: &Path, profile: &str, chat: &str) -> Result<Option<String>, String> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| "IM session store lock is poisoned")?;
    selection(&load(path)?, profile, chat).map(|selected| selected.map(str::to_owned))
}

/// Unknown slash commands remain normal user prompts.
pub fn command(
    path: &Path,
    profile: &str,
    chat: &str,
    text: &str,
) -> Option<Result<String, String>> {
    let words: Vec<_> = text.split_whitespace().collect();
    let command = *words.first()?;
    if !matches!(command, "/new" | "/sessions" | "/session") {
        return None;
    }
    Some(handle(path, profile, chat, &words))
}

fn handle(path: &Path, profile: &str, chat: &str, words: &[&str]) -> Result<String, String> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| "IM session store lock is poisoned")?;
    let mut value = load(path)?;
    let base = super::conversation_session_id(chat);
    let current = selection(&value, profile, chat)?
        .unwrap_or(&base)
        .to_owned();
    let entry_key = key(profile, chat);
    if value["chats"][&entry_key].is_null() {
        value["chats"][&entry_key] = json!({"selected":base,"sessions":[base]});
    }
    let entry = &mut value["chats"][&entry_key];
    let response = match words {
        ["/session"] => return Ok(format!("当前 session：{current}\n/new 新建会话\n/sessions 查看会话列表\n/session <ID> 切换会话")),
        ["/sessions"] => {
            let known = entry["sessions"].as_array().ok_or("invalid IM session list")?;
            let mut lines = vec!["当前聊天的 sessions（最近 20 个）：".to_owned()];
            for id in known.iter().rev().take(20).filter_map(Value::as_str) {
                lines.push(format!("{} {id}", if id == current { "当前 →" } else { "•" }));
            }
            lines.push("发送 /session <ID> 切换；/new 新建空白会话。".into());
            return Ok(lines.join("\n"));
        }
        ["/new"] => {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos();
            let id = format!("{base}-{stamp:x}");
            entry["selected"] = json!(id);
            entry["sessions"].as_array_mut().ok_or("invalid IM session list")?.push(json!(id));
            format!("已新建并切换 session：{id}\n后续消息使用空白上下文，原会话历史已保留。")
        }
        ["/session", id] => {
            if !entry["sessions"].as_array().ok_or("invalid IM session list")?.iter().any(|known| known.as_str() == Some(id)) {
                return Err("未找到当前聊天的这个 session。发送 /sessions 查看完整 ID。".into());
            }
            entry["selected"] = json!(id);
            format!("已切换 session：{id}\n后续消息使用该会话上下文。")
        }
        _ => return Err("指令用法：/new、/sessions、/session 或 /session <ID>。".into()),
    };
    let parent = path.parent().ok_or("invalid IM session store path")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let stage = path.with_extension("json.tmp");
    std::fs::write(
        &stage,
        serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::rename(&stage, path)
        .map_err(|error| format!("failed saving IM session selection: {error}"))?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupted_selection_is_reported_instead_of_resetting_history() {
        let path = std::env::temp_dir().join(format!(
            "rpi-im-sessions-invalid-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"{invalid").unwrap();
        assert!(selected(&path, "p", "oc_a").is_err());
        assert!(command(&path, "p", "oc_a", "/new").unwrap().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{invalid");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn creates_switches_persists_and_isolates_chat_sessions() {
        let path = std::env::temp_dir().join(format!(
            "rpi-im-sessions-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(selected(&path, "p", "oc_a").unwrap().is_none());
        assert!(command(&path, "p", "oc_a", "/session")
            .unwrap()
            .unwrap()
            .contains("im-feishu-oc_a"));
        command(&path, "p", "oc_a", "/new").unwrap().unwrap();
        let first = selected(&path, "p", "oc_a").unwrap().unwrap();
        command(&path, "p", "oc_a", "/new").unwrap().unwrap();
        assert_ne!(selected(&path, "p", "oc_a").unwrap().unwrap(), first);
        assert!(command(&path, "p", "oc_a", "/sessions")
            .unwrap()
            .unwrap()
            .contains(&first));
        command(&path, "p", "oc_a", &format!("/session {first}"))
            .unwrap()
            .unwrap();
        assert_eq!(selected(&path, "p", "oc_a").unwrap().unwrap(), first);
        assert!(command(&path, "p", "oc_b", &format!("/session {first}"))
            .unwrap()
            .is_err());
        assert!(selected(&path, "other-profile", "oc_a").unwrap().is_none());
        command(&path, "p", "oc_a", "/session im-feishu-oc_a")
            .unwrap()
            .unwrap();
        assert_eq!(
            selected(&path, "p", "oc_a").unwrap().unwrap(),
            "im-feishu-oc_a"
        );
        assert!(command(&path, "p", "oc_a", "/session ../../secret")
            .unwrap()
            .is_err());
        assert!(command(&path, "p", "oc_a", "/new extra").unwrap().is_err());
        assert!(command(&path, "p", "oc_a", "普通消息 /new").is_none());
        std::fs::remove_file(path).unwrap();
    }
}
