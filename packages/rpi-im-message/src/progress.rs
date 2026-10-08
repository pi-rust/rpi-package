//! Tool progress shared by the host event bridge and JSONL child process.
use serde_json::Value;

#[derive(Default)]
pub struct Progress {
    tools: Vec<Tool>,
}

struct Tool {
    id: String,
    name: String,
    preview: String,
    status: &'static str,
}

pub struct Update {
    pub tool_call_id: String,
    pub text: String,
}

impl Tool {
    fn update(&self) -> Update {
        Update {
            tool_call_id: self.id.clone(),
            text: format!(
                "{} {}{}{}",
                self.status,
                self.name,
                if self.preview.is_empty() { "" } else { "：" },
                self.preview
            ),
        }
    }
}

impl Progress {
    pub fn finish(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();
        for tool in &mut self.tools {
            if tool.status == "⏳" {
                tool.status = "⚠️ 未完成";
                updates.push(tool.update());
            }
        }
        updates
    }

    pub fn event(&mut self, event: &Value) -> Option<Update> {
        let kind = event.get("type")?.as_str()?;
        if !matches!(kind, "tool_execution_start" | "tool_execution_end") {
            return None;
        }
        let id = event.get("toolCallId")?.as_str()?;
        let status = if kind == "tool_execution_start" {
            "⏳"
        } else if event
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "❌"
        } else {
            "✅"
        };
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.id == id) {
            if tool.status == status || kind == "tool_execution_start" {
                return None;
            }
            tool.status = status;
            Some(tool.update())
        } else {
            let args = &event["args"];
            let preview = ["command", "path", "file_path", "query", "url", "pattern"]
                .iter()
                .find_map(|key| args.get(key).and_then(Value::as_str))
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let tool = Tool {
                id: id.to_owned(),
                name: event.get("toolName")?.as_str()?.chars().take(80).collect(),
                preview: preview.chars().take(80).collect(),
                status,
            };
            let update = tool.update();
            self.tools.push(tool);
            Some(update)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repeated_tool_names_keep_separate_messages_and_completion_targets() {
        let mut progress = Progress::default();
        for id in ["first", "second"] {
            let update = progress
                .event(&json!({"type":"tool_execution_start", "toolCallId":id,
                "toolName":"bash", "args":{"command":id}}))
                .unwrap();
            assert_eq!(update.tool_call_id, id);
            assert_eq!(update.text, format!("⏳ bash：{id}"));
        }
        // Parallel calls may complete in the reverse order.
        let update = progress
            .event(&json!({"type":"tool_execution_end", "toolCallId":"second",
            "toolName":"bash", "isError":false}))
            .unwrap();
        assert_eq!(update.tool_call_id, "second");
        assert_eq!(update.text, "✅ bash：second");
        let remaining = progress.finish();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].tool_call_id, "first");
        assert_eq!(remaining[0].text, "⚠️ 未完成 bash：first");
    }

    #[test]
    fn emits_each_tool_separately_and_updates_its_own_status() {
        let mut progress = Progress::default();
        let start = json!({"type":"tool_execution_start", "toolCallId":"a", "toolName":"bash", "args":{"command":"ls\n-la", "token":"secret"}});
        assert_eq!(progress.event(&start).unwrap().text, "⏳ bash：ls -la");
        assert!(progress.event(&start).is_none());
        let end = json!({"type":"tool_execution_end", "toolCallId":"a", "toolName":"bash", "isError":true});
        assert_eq!(progress.event(&end).unwrap().text, "❌ bash：ls -la");
        let next = json!({"type":"tool_execution_start", "toolCallId":"b", "toolName":"read", "args":{"path":"a.rs"}});
        let update = progress.event(&next).unwrap();
        assert_eq!(update.tool_call_id, "b");
        assert_eq!(update.text, "⏳ read：a.rs");
        assert!(!update.text.contains("secret"));
        assert!(progress.event(&json!({"type":"message_update"})).is_none());
        let updates = progress.finish();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].tool_call_id, "b");
        assert_eq!(updates[0].text, "⚠️ 未完成 read：a.rs");
        assert!(progress.finish().is_empty());
    }
}
