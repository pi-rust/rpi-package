//! Request execution: one absolute pre-response deadline, cancellation and one terminal event.
use super::{
    sse::DecoderState,
    stream::{EventSink, Flow, State},
};
use serde_json::Value;
use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub const PRE_RESPONSE_MS: u64 = 600_000;
pub const TIMEOUT_MESSAGE: &str = "OpenAI Codex did not begin a response within the pre-response time limit. Check your connection and try again later.";
const ABORTED: &str = "Request was aborted";

/// One absolute deadline; accepting headers never replenishes the remaining time.
pub struct Deadline {
    caller: CancellationToken,
    pub signal: CancellationToken,
    expires: Instant,
    active: AtomicBool,
    changed: tokio::sync::Notify,
}
impl Deadline {
    pub fn new(caller: CancellationToken) -> Self {
        Self {
            signal: caller.child_token(),
            caller,
            expires: Instant::now() + Duration::from_millis(PRE_RESPONSE_MS),
            active: AtomicBool::new(true),
            changed: tokio::sync::Notify::new(),
        }
    }
    pub fn check(&self) -> Result<(), String> {
        if self.caller.is_cancelled() {
            return Err(ABORTED.into());
        }
        if self.active.load(Ordering::Relaxed) && Instant::now() >= self.expires {
            self.signal.cancel();
            return Err(TIMEOUT_MESSAGE.into());
        }
        if self.signal.is_cancelled() {
            return Err(ABORTED.into());
        }
        Ok(())
    }
    pub fn accept_headers(&self) -> Result<(), String> {
        self.check()?;
        self.active.store(false, Ordering::Relaxed);
        self.changed.notify_waiters();
        Ok(())
    }
    pub async fn wait<T>(
        &self,
        work: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        tokio::pin!(work);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.check()?;
            let result = tokio::select! {
                biased;
                _=self.caller.cancelled()=>Err(ABORTED.into()),
                _=tokio::time::sleep_until(self.expires), if self.active.load(Ordering::Relaxed)=>{
                    self.signal.cancel();
                    Err(TIMEOUT_MESSAGE.into())
                },
                _=self.signal.cancelled()=>Err(ABORTED.into()),
                _=changed=>continue,
                value=&mut work=>value,
            };
            self.check()?;
            return result;
        }
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        self.signal.cancel();
    }
}

#[derive(Default)]
pub struct Secrets {
    pub credentials: Vec<String>,
    pub accounts: Vec<String>,
}
impl Secrets {
    pub fn credential(&mut self, value: &str) {
        if !value.is_empty() && !self.credentials.iter().any(|known| known == value) {
            self.credentials.push(value.into());
        }
    }
    pub fn account(&mut self, value: &str) {
        if !value.is_empty() && !self.accounts.iter().any(|known| known == value) {
            self.accounts.push(value.into());
        }
    }
}
pub trait Body: Send {
    fn next(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}
pub struct Response<B> {
    pub status: u16,
    pub body: B,
}
/// Sources resolve credentials at each physical send; the stream never falls back between sources.
pub trait Source: Sync {
    type Body: Body;
    fn now_ms(&self) -> i64;
    async fn open(
        &self,
        model: &Value,
        context: &Value,
        options: &Value,
        deadline: &Deadline,
        secrets: &mut Secrets,
    ) -> Result<Response<Self::Body>, String>;
}

/// Runs one logical request. Exactly one terminal event reaches `sink`.
pub async fn run(
    source: &impl Source,
    model: &Value,
    context: &Value,
    options: &Value,
    caller: CancellationToken,
    sink: &mut impl EventSink,
) {
    let deadline = Deadline::new(caller.clone());
    let mut state = State::new(model, source.now_ms());
    let mut secrets = Secrets::default();
    if let Some(key) = options["apiKey"].as_str() {
        secrets.credential(key);
    }
    let result = async {
        state.start(sink).await?;
        // TLS futures are large in debug builds; keep nested deadline frames off the thread stack.
        let mut response = deadline
            .wait(Box::pin(source.open(
                model,
                context,
                options,
                &deadline,
                &mut secrets,
            )))
            .await?;
        if (200..300).contains(&response.status) {
            deadline.accept_headers()?;
        }
        if !(200..300).contains(&response.status) {
            let text = deadline
                .wait(async {
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response.body.next().await? {
                        bytes.extend(chunk);
                    }
                    Ok(encoding_rs::UTF_8.decode(&bytes).0.into_owned())
                })
                .await?;
            let detail = error_detail(&text);
            return Err(format!(
                "OpenAI Codex request failed: HTTP {} {detail}",
                response.status
            ));
        }
        let mut decoder = DecoderState::default();
        let mut eof = false;
        loop {
            let next = async {
                loop {
                    if let Some(event) = decoder.next_event(eof) {
                        return Ok::<_, String>(Some(event));
                    }
                    if eof {
                        return Ok(None);
                    }
                    match response.body.next().await? {
                        Some(bytes) => decoder.push(&bytes),
                        None => eof = true,
                    }
                }
            };
            let Some(event) = (match deadline.wait(next).await {
                Ok(chunk) => chunk,
                // Shutdown can tear the connection down before the caller token reaches
                // this task; a read error racing with cancellation is an abort.
                Err(error) => {
                    if caller.is_cancelled() {
                        return Err(ABORTED.into());
                    }
                    return Err(error);
                }
            }) else {
                break;
            };
            if state.event(&event, sink).await? == Flow::Complete {
                break;
            }
        }
        drop(response);
        state.finish(caller.is_cancelled(), sink).await?;
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = result {
        deadline.signal.cancel();
        let _ = state
            .fail(
                &error,
                caller.is_cancelled(),
                &secrets.credentials,
                &secrets.accounts,
                sink,
            )
            .await;
    }
}

/// Prefers the JSON error message OpenAI backends return, keeping the raw body otherwise.
fn error_detail(text: &str) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => {
            let message = value["error"]["message"]
                .as_str()
                .filter(|message| !message.is_empty());
            let code = value["error"]["code"]
                .as_str()
                .filter(|code| !code.is_empty());
            match (code, message) {
                (Some(code), Some(message)) => format!("{code}: {message}"),
                (None, Some(message)) => message.to_owned(),
                _ => text.to_owned(),
            }
        }
        Err(_) => text.to_owned(),
    }
}
