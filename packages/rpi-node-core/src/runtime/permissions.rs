//! ACP permission conversion. Never infer an option ID or an allow decision.
use serde_json::{Value, json};

use super::RuntimeError;
#[cfg(any(not(target_arch = "wasm32"), test))]
use super::dialog_from_wire;
#[cfg(test)]
use rpi_node_api::api::PendingDialog;
use rpi_node_api::api::WireEvent;

fn invalid_permission() -> RuntimeError {
    RuntimeError::Acp("invalid or ambiguous ACP permission selection".into())
}

/// Preserve original protocol metadata, while adapting presentation to `AskCard`.
#[cfg(any(not(target_arch = "wasm32"), test))]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn permission_event(
    request_id: Value,
    params: Value,
) -> Result<WireEvent, RuntimeError> {
    if !request_id.is_string() && !request_id.is_number() {
        return Err(invalid_permission());
    }
    let key = request_id
        .as_str()
        .map_or_else(|| request_id.to_string(), str::to_owned);
    let presentation = params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .map(|option| {
                    json!({
                        "title": option.get("name"),
                        "permission": option.get("kind"),
                        "optionId": option.get("optionId")
                    })
                })
                .collect::<Vec<_>>()
        });
    let event = super::transport::wire_event(
        Some(key.clone()),
        None,
        "ui_dialog_request",
        json!({
            "type":"ui_dialog_request", "requestId":key,
            "jsonRpcRequestId":request_id, "sessionId":params.get("sessionId"),
            "toolCallId": params.pointer("/toolCall/toolCallId"),
            "ui": {
                "purpose":"permission", "kind":"selector",
                "question":params.pointer("/toolCall/title"),
                "options":presentation, "acpPermission":params
            }
        }),
    );
    // Reject non-object UI shapes here, not in a later stream consumer.
    dialog_from_wire(&event)?;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> PendingDialog {
        dialog_from_wire(
            &permission_event(
                json!("4"),
                json!({
                    "sessionId":"s", "toolCall":{"toolCallId":"t","title":"Run tool?"},
                    "options":[
                        {"optionId":"deny-7","name":"No","kind":"reject_once"},
                        {"optionId":"permit-9","name":"Yes","kind":"allow_once"}
                    ]
                }),
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn selects_exact_id_or_cancels_without_inference() {
        let dialog = dialog();
        assert_eq!(dialog.response_id, Some(json!("4")));
        assert_eq!(dialog.ui.purpose.as_deref(), Some("permission"));
        assert_eq!(dialog.tool_call_id.as_deref(), Some("t"));
        for (label, id) in [("No", "deny-7"), ("Yes", "permit-9")] {
            assert_eq!(
                dialog
                    .acp_permission_result(Some(&json!({"kind":"selector","values":[label]})))
                    .unwrap(),
                json!({"outcome":{"outcome":"selected","optionId":id}})
            );
        }
        assert_eq!(
            dialog.acp_permission_policy(true).unwrap(),
            json!({"outcome":{"outcome":"selected","optionId":"permit-9"}})
        );
        assert_eq!(
            dialog.acp_permission_policy(false).unwrap(),
            json!({"outcome":{"outcome":"cancelled"}})
        );
        for answer in [
            json!({}),
            json!({"kind":"input","values":["Yes"]}),
            json!({"kind":"selector","values":[]}),
            json!({"kind":"selector","values":["Yes","No"]}),
            json!({"kind":"selector","values":["allow"]}),
        ] {
            assert!(dialog.acp_permission_result(Some(&answer)).is_err());
        }
    }

    #[test]
    fn rejects_duplicate_names_ids_unknown_kinds_and_missing_metadata() {
        let mut dialog = dialog();
        dialog.ui.raw["acpPermission"]["options"][1]["name"] = json!("No");
        assert!(
            dialog
                .acp_permission_result(Some(&json!({"kind":"selector","values":["No"]})))
                .is_err()
        );
        dialog.ui.raw["acpPermission"]["options"][1]["optionId"] = json!("deny-7");
        assert!(dialog.acp_permission_policy(true).is_err());
        dialog.ui.raw["acpPermission"]["options"] =
            json!([{"optionId":"x","name":"Yes","kind":"unknown"}]);
        assert!(
            dialog
                .acp_permission_result(Some(&json!({"kind":"selector","values":["Yes"]})))
                .is_err()
        );
        assert_eq!(
            dialog.acp_permission_policy(true).unwrap(),
            json!({"outcome":{"outcome":"cancelled"}})
        );
        dialog.response_id = None;
        assert!(dialog.acp_permission_result(None).is_err());
    }
}
