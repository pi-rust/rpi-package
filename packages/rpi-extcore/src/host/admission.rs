//! Local execution intent exists before command publication. Correlated native
//! rejection can settle it; timeout, acceptance and caller loss cannot. Writer
//! publication and unsent cancellation share one mutex so an unsent rollback
//! can never race a later write of the same command.
use super::*;
#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct ExecutionAdmission {
    current: Option<Intent>,
    native_unsettled: bool,
    native_run: Option<String>,
}
struct Intent {
    id: String,
    token: String,
    published: bool,
    accepted: bool,
    run: Option<String>,
}
pub(super) struct ExecutionTicket {
    state: Arc<StdMutex<ExecutionAdmission>>,
    token: String,
    cancel_queued: bool,
}
impl Clone for ExecutionTicket {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            token: self.token.clone(),
            cancel_queued: true,
        }
    }
}
impl ExecutionTicket {
    pub(super) fn register(
        state: &Arc<StdMutex<ExecutionAdmission>>,
        value: &Value,
    ) -> Result<Option<Self>, String> {
        if !matches!(
            value["type"].as_str(),
            Some("prompt" | "compact" | "resume_pending")
        ) {
            return Ok(None);
        }
        let id = value["id"]
            .as_str()
            .ok_or("execution requires a string request ID")?
            .to_owned();
        let mut admission = state.lock().expect("execution admission");
        if admission.unsettled() {
            return Err("execution admission remains unsettled".into());
        }
        let token = uuid::Uuid::new_v4().to_string();
        admission.current = Some(Intent {
            id,
            token: token.clone(),
            published: false,
            accepted: false,
            run: None,
        });
        Ok(Some(Self {
            state: state.clone(),
            token,
            cancel_queued: true,
        }))
    }
    pub(super) fn published(&self) -> bool {
        let mut admission = self.state.lock().expect("execution admission");
        if let Some(intent) = admission
            .current
            .as_mut()
            .filter(|intent| intent.token == self.token)
        {
            intent.published = true;
            true
        } else {
            false
        }
    }
    pub(super) fn retain_queued(&mut self) {
        self.cancel_queued = false;
    }
}
impl Drop for ExecutionTicket {
    fn drop(&mut self) {
        if !self.cancel_queued {
            return;
        }
        let mut admission = self.state.lock().expect("execution admission");
        if admission
            .current
            .as_ref()
            .is_some_and(|intent| intent.token == self.token && !intent.published)
        {
            admission.current = None;
        }
    }
}
fn matches_run(expected: Option<&str>, event: Option<&str>) -> bool {
    match expected {
        Some(expected) => event == Some(expected),
        None => true,
    }
}
impl ExecutionAdmission {
    pub(super) fn unsettled(&self) -> bool {
        self.current.is_some() || self.native_unsettled
    }
    pub(super) fn observe(&mut self, value: &Value) {
        let run = value["runId"].as_str();
        match value["type"].as_str() {
            Some("response") => {
                let Some(intent) = self
                    .current
                    .as_mut()
                    .filter(|intent| Some(intent.id.as_str()) == value["id"].as_str())
                else {
                    return;
                };
                if value["success"] == false && !intent.accepted {
                    self.current = None;
                } else if value["success"] == true && value["data"]["accepted"] == true {
                    intent.accepted = true;
                    self.native_unsettled = true;
                    self.native_run = intent.run.clone();
                }
            }
            Some("agent_start") => {
                self.native_unsettled = true;
                self.native_run = run.map(str::to_owned);
                if let Some(intent) = self.current.as_mut().filter(|intent| intent.published) {
                    intent.run = self.native_run.clone();
                }
            }
            Some("agent_settled") => {
                if matches_run(self.native_run.as_deref(), run) {
                    self.native_unsettled = false;
                    self.native_run = None;
                }
                if self.current.as_ref().is_some_and(|intent| {
                    intent.accepted && matches_run(intent.run.as_deref(), run)
                }) {
                    self.current = None;
                }
            }
            _ => {}
        }
    }
}
