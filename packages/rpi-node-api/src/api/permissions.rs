//! Pure ACP permission conversion. Never infer an option ID or an allow decision.
use serde_json::{Value, json};

use super::{AcpPermissionError, PendingDialog};

fn invalid_permission() -> AcpPermissionError {
    AcpPermissionError
}

fn options(dialog: &PendingDialog) -> Result<&[Value], AcpPermissionError> {
    dialog
        .ui
        .raw
        .pointer("/acpPermission/options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(invalid_permission)
}

fn selected_result(option: &Value) -> Result<Value, AcpPermissionError> {
    let id = option
        .get("optionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(invalid_permission)?;
    if !matches!(
        option.get("kind").and_then(Value::as_str),
        Some("allow_once" | "allow_always" | "reject_once" | "reject_always")
    ) {
        return Err(invalid_permission());
    }
    Ok(json!({"outcome":{"outcome":"selected","optionId":id}}))
}

impl PendingDialog {
    /// Convert the single selected UI label back to the exact negotiated ACP ID.
    /// Invalid, unknown, multi-select or ambiguous answers are errors, not allow.
    ///
    /// # Errors
    /// Rejects missing response metadata or an invalid/ambiguous selection.
    pub fn acp_permission_result(
        &self,
        answer: Option<&Value>,
    ) -> Result<Value, AcpPermissionError> {
        if self.response_id.is_none() {
            return Err(invalid_permission());
        }
        let Some(answer) = answer else {
            return Ok(json!({"outcome":{"outcome":"cancelled"}}));
        };
        if answer.get("kind").and_then(Value::as_str) != Some("selector") {
            return Err(invalid_permission());
        }
        let values = answer
            .get("values")
            .and_then(Value::as_array)
            .filter(|values| values.len() == 1)
            .ok_or_else(invalid_permission)?;
        let label = values[0].as_str().ok_or_else(invalid_permission)?;
        let mut matching = options(self)?
            .iter()
            .filter(|option| option.get("name").and_then(Value::as_str) == Some(label));
        let option = matching.next().ok_or_else(invalid_permission)?;
        if matching.next().is_some() {
            return Err(invalid_permission());
        }
        let result = selected_result(option)?;
        self.validate_acp_permission_result(&result)?;
        Ok(result)
    }

    /// Non-interactive opt-in chooses an actual allow option; absent one, cancel.
    ///
    /// This only constructs an answer payload; the ACP adapter must separately
    /// verify the outstanding request and its original response ID.
    ///
    /// # Errors
    /// Rejects invalid/ambiguous options or cancellation without response metadata.
    pub fn acp_permission_policy(&self, allow: bool) -> Result<Value, AcpPermissionError> {
        if allow {
            for option in options(self)? {
                if matches!(
                    option.get("kind").and_then(Value::as_str),
                    Some("allow_once" | "allow_always")
                ) {
                    let result = selected_result(option)?;
                    self.validate_acp_permission_result(&result)?;
                    return Ok(result);
                }
            }
        }
        self.acp_permission_result(None)
    }

    /// Validate the selected negotiated ID. This is payload validation, not authorization.
    ///
    /// # Errors
    /// Rejects unknown, ambiguous or malformed selections.
    pub fn validate_acp_permission_result(&self, result: &Value) -> Result<(), AcpPermissionError> {
        let outcome = result.get("outcome").ok_or_else(invalid_permission)?;
        match outcome.get("outcome").and_then(Value::as_str) {
            Some("cancelled") if outcome.get("optionId").is_none() => Ok(()),
            Some("selected") => {
                let id = outcome
                    .get("optionId")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_permission)?;
                let mut matching = options(self)?
                    .iter()
                    .filter(|option| option.get("optionId").and_then(Value::as_str) == Some(id));
                let option = matching.next().ok_or_else(invalid_permission)?;
                if matching.next().is_some() {
                    return Err(invalid_permission());
                }
                selected_result(option).map(|_| ())
            }
            _ => Err(invalid_permission()),
        }
    }
}
