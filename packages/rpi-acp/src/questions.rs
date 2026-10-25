//! ACP elicitation mapping for `ask_user`; permissions keep their standard method.
use serde_json::{json, Value};

/// Standard ACP v1 agent-to-client question request.
pub const ELICITATION_METHOD: &str = "elicitation/create";
/// Compatibility extension used by older rpi ACP clients.
pub const REQUEST_METHOD: &str = "_rpi/request_question";
pub const CLOSED_METHOD: &str = "_rpi/dialog_closed";
const MAX_TEXT: usize = 65_536;

fn validated(event: &Value) -> Option<(Value, String, String, Vec<String>)> {
    let ui = event.get("ui")?;
    if !matches!(
        ui.get("purpose").and_then(Value::as_str),
        None | Some("question")
    ) {
        return None;
    }
    rpi_plugin_sdk::dialogs::validate(ui, event.get("toolCallId").and_then(Value::as_str)).ok()?;
    let kind = ui.get("kind")?.as_str()?;
    if !matches!(kind, "input" | "selector" | "confirm") {
        return None;
    }
    let question = ui.get("question").or_else(|| ui.get("message"))?.as_str()?;
    if question.trim().is_empty() || question.len() > MAX_TEXT {
        return None;
    }
    let request_id = event.get("requestId")?.as_str()?;
    if request_id.is_empty() {
        return None;
    }
    let options = titles(ui)?
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if kind != "input" && options.is_empty() {
        return None;
    }
    Some((
        ui.clone(),
        request_id.to_owned(),
        question.to_owned(),
        options,
    ))
}

/// Build the legacy rpi question-extension request.
pub fn request(event: &Value, session_id: &str) -> Option<Value> {
    let (ui, request_id, _, _) = validated(event)?;
    Some(json!({"sessionId":session_id, "requestId":request_id,
        "toolCallId":event.get("toolCallId"), "ui":ui}))
}

/// Build a standard ACP `elicitation/create` form request.
///
/// The native dialog uses a single `answer` string for input/single-choice
/// questions and an `answers` array for multiple choice. This shape matches
/// ACP's JSON-Schema form model and is rendered by Zed's elicitation UI.
pub fn elicitation_request(event: &Value, session_id: &str) -> Option<Value> {
    let (ui, request_id, question, options) = validated(event)?;
    let kind = ui.get("kind").and_then(Value::as_str).unwrap_or("input");
    let allow_multiple = ui
        .get("allowMultiple")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let allow_freeform = ui
        .get("allowFreeform")
        .and_then(Value::as_bool)
        .unwrap_or(kind == "input");
    let allow_comment = ui
        .get("allowComment")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let mut answer_property = if allow_multiple {
        json!({
            "type": "array",
            "title": "Answer",
            "items": {"type": "string"},
            "minItems": 1,
            "maxItems": 64
        })
    } else {
        json!({"type": "string", "title": "Answer"})
    };
    if !options.is_empty() && !allow_freeform {
        if allow_multiple {
            answer_property["items"]["enum"] = json!(options);
        } else {
            answer_property["enum"] = json!(options);
        }
    }
    if !allow_multiple && (options.is_empty() || allow_freeform) {
        if let Some(suggest) = ui.get("suggest").and_then(Value::as_str) {
            if !suggest.is_empty() {
                answer_property["default"] = json!(suggest);
            }
        }
    }
    let answer_name = if allow_multiple { "answers" } else { "answer" };
    properties.insert(answer_name.to_owned(), answer_property);
    required.push(answer_name);

    if allow_comment {
        properties.insert(
            "comment".to_owned(),
            json!({"type":"string", "title":"Comment"}),
        );
    }

    let mut message = question;
    if let Some(header) = ui.get("header").and_then(Value::as_str) {
        if !header.trim().is_empty() {
            message = format!("{header}: {message}");
        }
    }
    if let Some(context) = ui.get("context").and_then(Value::as_str) {
        if !context.trim().is_empty() {
            message.push_str("\n\n");
            message.push_str(context);
        }
    }
    Some(json!({
        "mode": "form",
        "sessionId": session_id,
        "toolCallId": event.get("toolCallId"),
        "message": message,
        "requestedSchema": {
            "type": "object",
            "properties": properties,
            "required": required
        },
        "_meta": {"rpi": {"requestId": request_id}}
    }))
}

/// Convert a standard ACP elicitation response to the native dialog response.
pub fn elicitation_answer(request_id: &str, ui: &Value, result: &Value) -> Value {
    if result.get("action").and_then(Value::as_str) != Some("accept") {
        return json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true});
    }
    let Some(content) = result.get("content").and_then(Value::as_object) else {
        return json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true});
    };
    let allow_multiple = ui
        .get("allowMultiple")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let values = if allow_multiple {
        content
            .get("answers")
            .or_else(|| content.get("answer"))
            .and_then(Value::as_array)
            .cloned()
    } else {
        content
            .get("answer")
            .and_then(Value::as_str)
            .map(|value| vec![json!(value)])
    };
    let Some(values) = values else {
        return json!({"type":"ui_dialog_response", "requestId":request_id, "cancelled":true});
    };
    let mut normalized = json!({"values": values});
    if let Some(comment) = content.get("comment") {
        normalized["text"] = comment.clone();
    }
    answer(request_id, ui, &json!({"answer": normalized}))
}

use rpi_extcore::dialogs::titles;
pub use rpi_extcore::dialogs::{answer, timeout};

#[cfg(test)]
mod tests {
    use super::*;
    fn ui() -> Value {
        json!({"kind":"selector","question":"语言？","options":[{"title":"Rust"},{"title":"Python"}],"allowMultiple":true,"allowComment":true})
    }
    #[test]
    fn multiple_choices_and_comment_survive() {
        assert_eq!(
            answer(
                "q",
                &ui(),
                &json!({"answer":{"values":["Rust","Python"],"text":"一起使用"}})
            ),
            json!({"type":"ui_dialog_response","requestId":"q","answer":{"values":["Rust","Python"],"text":"一起使用"}})
        );
    }
    #[test]
    fn invalid_and_duplicate_choices_are_cancelled() {
        for values in [
            json!(["Go"]),
            json!(["Rust", "Rust"]),
            json!([1]),
            json!([]),
        ] {
            assert_eq!(
                answer("q", &ui(), &json!({"answer":{"values":values}}))["cancelled"],
                true
            );
        }
        let mut single = ui();
        single["allowMultiple"] = json!(false);
        assert_eq!(
            answer(
                "q",
                &single,
                &json!({"answer":{"values":["Rust","Python"]}})
            )["cancelled"],
            true
        );
    }
    #[test]
    fn input_and_freeform_are_preserved() {
        let input = json!({"kind":"input","question":"端口？"});
        assert_eq!(
            answer("q", &input, &json!({"answer":{"values":["7897"]}}))["answer"],
            json!({"values":["7897"],"text":"7897"})
        );
        let mut custom = ui();
        custom["allowFreeform"] = json!(true);
        assert_eq!(
            answer("q", &custom, &json!({"answer":{"values":["Go"]}}))["answer"]["values"],
            json!(["Go"])
        );
    }
    #[test]
    fn permission_and_unknown_ui_never_use_question_extension() {
        let mut event = json!({"requestId":"q","ui":ui()});
        assert!(request(&event, "s").is_some());
        event["ui"]["purpose"] = json!("permission");
        assert!(request(&event, "s").is_none());
        event["ui"]["purpose"] = json!("future");
        assert!(request(&event, "s").is_none());
        event["ui"].as_object_mut().unwrap().remove("purpose");
        event["ui"]["kind"] = json!("message");
        assert!(request(&event, "s").is_none());
    }
    #[test]
    fn comments_and_input_text_cannot_override_an_unrelated_choice() {
        let selector = json!({"kind":"selector", "options":["Yes","No"]});
        assert_eq!(
            answer("q", &selector, &json!({"answer":{"values":["No"]}}))["answer"]["values"],
            json!(["No"])
        );
        assert_eq!(
            answer(
                "q",
                &selector,
                &json!({"answer":{"values":["Yes"],"text":"extra"}})
            )["cancelled"],
            true
        );
        let input = json!({"kind":"input"});
        assert_eq!(
            answer(
                "q",
                &input,
                &json!({"answer":{"values":["one"],"text":"two"}})
            )["cancelled"],
            true
        );
    }
    #[test]
    fn standard_selector_request_and_answer_match_acp_form_shape() {
        let event = json!({
            "requestId":"q1", "toolCallId":"ask-1",
            "ui": {"kind":"selector", "header":"语言", "question":"请选择", "context":"请选择", "options":[{"title":"Rust"},{"title":"Python"}]}
        });
        let request = elicitation_request(&event, "session-1").unwrap();
        assert_eq!(request["mode"], "form");
        assert_eq!(request["sessionId"], "session-1");
        assert_eq!(request["message"], "语言: 请选择\n\n请选择");
        assert_eq!(
            request["requestedSchema"]["properties"]["answer"]["enum"],
            json!(["Rust", "Python"])
        );
        assert_eq!(
            elicitation_answer(
                "q1",
                &event["ui"],
                &json!({"action":"accept","content":{"answer":"Rust"}})
            ),
            json!({"type":"ui_dialog_response","requestId":"q1","answer":{"values":["Rust"]}})
        );
    }
    #[test]
    fn standard_multiple_and_input_responses_are_normalized() {
        let mut multiple = ui();
        multiple["allowMultiple"] = json!(true);
        assert_eq!(
            elicitation_answer(
                "q",
                &multiple,
                &json!({"action":"accept","content":{"answers":["Rust","Python"],"comment":"both"}})
            ),
            json!({"type":"ui_dialog_response","requestId":"q","answer":{"values":["Rust","Python"],"text":"both"}})
        );
        let input = json!({"kind":"input","question":"端口？"});
        assert_eq!(
            elicitation_answer(
                "q",
                &input,
                &json!({"action":"accept","content":{"answer":"7897"}})
            ),
            json!({"type":"ui_dialog_response","requestId":"q","answer":{"values":["7897"],"text":"7897"}})
        );
        assert_eq!(
            elicitation_answer("q", &input, &json!({"action":"decline"}))["cancelled"],
            true
        );
    }
}
