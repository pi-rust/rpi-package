//! Bound session observations and typed permission replies, not authorization.
use super::{LocalError, LocalWorkspace, WorkspaceError, decode, local, service};
use rpi_node_api::api::{SessionAccess, SessionSummary};
use serde_json::{Value, json};

impl LocalWorkspace {
    /// Observe the live command intersection, never a configuration-only subset.
    pub async fn session_access(&self, id: &str) -> Result<SessionAccess, WorkspaceError> {
        let _lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if self.inner.agents.contains(id)? {
            let agent = self.inner.agents.prepare(id, false).await?;
            let descriptor = agent.descriptor();
            return Ok(SessionAccess {
                dialog_reply: descriptor.supports("dialogs"),
                descriptor,
                supported_commands: ["prompt", "abort", "cancel"]
                    .into_iter()
                    .filter(|command| agent.supports_command(command))
                    .map(str::to_owned)
                    .collect(),
                snapshot_limit: false,
                prompt_content: None,
            });
        }
        let service = self.session_service(id)?;
        let _: SessionSummary = decode(service::open_session(&service, id.into()).await)?;
        let host = service.live_session(id).await.ok_or_else(|| {
            WorkspaceError::Local(LocalError::NotFound(format!("live session {id}")))
        })?;
        let supported_commands: Vec<String> = super::command_allowlist::allowed_commands()
            .filter(|command| host.supports_command(command))
            .map(str::to_owned)
            .collect();
        Ok(SessionAccess {
            descriptor: self.descriptor(),
            dialog_reply: supported_commands
                .iter()
                .any(|command| command == "ui_dialog_response"),
            // This is a service capability, not a raw-command advertisement.
            // extcore snapshot_with_options always supports a bounded snapshot;
            // older hosts may handle get_messages without advertising it.
            snapshot_limit: true,
            prompt_content: {
                let profile = host.ready()["promptContent"].clone();
                rpi_extcore::prompt::PromptProfile::from_ready(&profile)
                    .ok()
                    .map(|_| profile)
            },
            supported_commands,
        })
    }

    /// None means RPI: the caller retains its original command behavior.
    pub(super) async fn agent_dialog_response(
        &self,
        id: &str,
        request_id: &str,
        answer: Option<&Value>,
    ) -> Result<Option<Value>, WorkspaceError> {
        let lease = self.inner.lifecycle.read().await;
        local(self.lock().map(|_| ()))?;
        if !self.inner.agents.contains(id)? {
            return Ok(None);
        }
        let agent = self.inner.agents.prepare(id, false).await?;
        let dialog = agent.pending_dialog(request_id)?;
        let result = dialog.acp_permission_result(answer).map_err(|_| {
            WorkspaceError::Local(LocalError::InvalidInput("invalid ACP dialog answer".into()))
        })?;
        let response_id = dialog.response_id.ok_or_else(|| {
            WorkspaceError::Local(LocalError::InvalidInput(
                "missing ACP dialog response binding".into(),
            ))
        })?;
        // Shutdown must remain able to stop an unresponsive child. The prepared
        // handle rechecks generation and the outstanding owner-side binding.
        drop(lease);
        agent.respond_to_agent_request(response_id, result).await?;
        Ok(Some(json!({"ok": true})))
    }
}

/// Raw host send replaces the correlation id with a UUID. Validate both the
/// supplied envelope and the exact-size UUID envelope before handing it off.
pub(super) fn validate_prompt_content(
    payload: &Value,
    profile: &Value,
) -> Result<(), WorkspaceError> {
    let invalid = |message| WorkspaceError::Local(LocalError::InvalidInput(message));
    let fields = payload
        .as_object()
        .ok_or_else(|| invalid("invalid ordered prompt".into()))?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "id" | "contentBlocks"))
    {
        return Err(invalid(
            "ordered prompt has unknown or legacy fields".into(),
        ));
    }
    let profile = rpi_extcore::prompt::PromptProfile::from_ready(profile)
        .map_err(|_| super::unsupported("prompt_content", "native prompt profile"))?;
    profile.validate_command(payload).map_err(invalid)?;
    let mut final_envelope = payload.clone();
    final_envelope["id"] = json!("00000000-0000-0000-0000-000000000000");
    profile.validate_command(&final_envelope).map_err(invalid)
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    fn profile() -> Value {
        json!({
            "contentBlocksVersion":1,"image":true,"imageMimeTypes":["image/png","image/jpeg"],
            "maxImages":4,"maxImageBytes":262144,"maxTotalImageBytes":524288,
            "maxImageBase64Bytes":349528,"maxPromptBytes":786432,"maxTextBytes":32768,
            "maxContentBlocks":64,"maxImageSide":2000,"maxImagePixels":4000000,
            "imageDecodeAllocBudgetBytes":33554432,"frameBytes":1048576
        })
    }
    fn command(blocks: Value) -> Value {
        json!({"type":"prompt","contentBlocks":blocks})
    }
    fn image() -> Value {
        json!({"type":"image","mimeType":"image/jpeg","data":"/9j/"})
    }

    #[test]
    fn ordered_prompt_validates_inline_structure_and_preserves_order() {
        let blocks =
            json!([{"type":"text","text":"caption"}, image(), {"type":"text","text":"attachment"}]);
        let payload = command(blocks.clone());
        validate_prompt_content(&payload, &profile()).unwrap();
        let content = rpi_extcore::prompt::PromptContent::from_value(&blocks).unwrap();
        assert!(content.has_images());
        assert_eq!(serde_json::to_value(content).unwrap(), blocks);
        validate_prompt_content(
            &command(json!([{"type":"text","text":"text only"}])),
            &profile(),
        )
        .unwrap();
    }

    #[test]
    fn unknown_fields_aliases_urls_paths_mimes_and_noncanonical_base64_fail() {
        for block in [
            json!({"type":"image","mimeType":"image/jpeg","data":"/9j/","path":"x"}),
            json!({"type":"image","mimeType":"image/jpeg","data":"file:///x"}),
            json!({"type":"image","mimeType":"image/jpeg","url":"https://example.com/x"}),
            json!({"type":"image","mimeType":"image/gif","data":"/9j/"}),
            json!({"type":"image","mimeType":"image/png","data":"/9j/"}),
            json!({"type":"image","mimeType":"image/jpeg","data":"/9j/\n"}),
            json!({"type":"image","mimeType":"image/jpeg","data":"_9j_"}),
            json!({"type":"image","mimeType":"image/jpeg","data":""}),
            json!({"type":"text","text":"x","unknown":true}),
            json!({"type":"tool_result","text":"x"}),
        ] {
            assert!(validate_prompt_content(&command(json!([block])), &profile()).is_err());
        }
        for key in ["message", "images", "content", "prompt", "unknown"] {
            let mut payload = command(json!([image()]));
            payload[key] = Value::Null;
            assert!(validate_prompt_content(&payload, &profile()).is_err());
        }
        for blocks in [Value::Null, json!({}), json!([])] {
            assert!(validate_prompt_content(&command(blocks), &profile()).is_err());
        }
    }

    #[test]
    fn missing_or_malformed_profile_gates_images_and_text_only() {
        let good = profile();
        let mut profiles = vec![Value::Null, json!(false), json!({"image":true})];
        for key in good.as_object().unwrap().keys() {
            let mut missing = good.clone();
            missing.as_object_mut().unwrap().remove(key);
            profiles.push(missing);
            let mut bad = good.clone();
            bad[key] = json!("incorrect");
            profiles.push(bad);
        }
        for profile in profiles {
            for blocks in [json!([image()]), json!([{"type":"text","text":"x"}])] {
                assert!(validate_prompt_content(&command(blocks), &profile).is_err());
            }
        }
    }

    #[test]
    fn aggregate_budgets_and_final_host_uuid_are_charged() {
        for blocks in [
            json!(vec![image(); 5]),
            json!(vec![json!({"type":"text","text":""}); 65]),
            json!([{"type":"text","text":"x".repeat(32769)}]),
            json!([{"type":"image","mimeType":"image/jpeg","data":"/9j/".repeat(87382)}]),
            json!(vec![
                json!({"type":"image","mimeType":"image/jpeg","data":"/9j/".repeat(87381)});
                3
            ]),
        ] {
            assert!(validate_prompt_content(&command(blocks), &profile()).is_err());
        }

        let large = json!({"type":"image","mimeType":"image/jpeg","data":"/9j/".repeat(87381)});
        let mut payload =
            command(json!([large.clone(), large, {"type":"text","text":"\0".repeat(14000)}]));
        let slack =
            rpi_extcore::prompt::MAX_PROMPT_BYTES - serde_json::to_vec(&payload).unwrap().len() - 1;
        payload["contentBlocks"][2]["text"] =
            json!(format!("{}{}", "\0".repeat(14000), "a".repeat(slack)));
        // The id-less frame fits exactly; HostSession::send's UUID would not.
        rpi_extcore::prompt::PromptProfile::from_ready(&profile())
            .unwrap()
            .validate_command(&payload)
            .unwrap();
        assert!(validate_prompt_content(&payload, &profile()).is_err());
        let mut huge_id = command(json!([image()]));
        huge_id["id"] = json!("x".repeat(rpi_extcore::prompt::MAX_PROMPT_BYTES));
        assert!(validate_prompt_content(&huge_id, &profile()).is_err());
    }
}
