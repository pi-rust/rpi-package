//! Protocol-neutral native authentication. Only the native executable handles credentials.
//! Environment overrides are child-scoped; inherited environment is never mutated.
use process_wrap::tokio::{TokioChildWrapper, TokioCommandWrap};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::ChildStdout,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{timeout_at, Instant},
};

const LIMIT: usize = 64 * 1024;
const OVERALL: Duration = Duration::from_secs(300);
const DESCRIBE: Duration = Duration::from_secs(5);

/// Executable and provider are explicit. `env` overlays the inherited environment.
#[derive(Clone)]
pub struct AuthOptions {
    pub executable: PathBuf,
    pub provider: String,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: Option<PathBuf>,
    /// Each interaction expires independently, capped by the fixed five-minute run limit.
    pub prompt_timeout: Duration,
}
impl AuthOptions {
    pub fn new(executable: impl Into<PathBuf>, provider: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            provider: provider.into(),
            env: BTreeMap::new(),
            cwd: None,
            prompt_timeout: Duration::from_secs(120),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    Oauth,
    ApiKey,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthDescriptor {
    pub rpc: bool,
    pub provider: String,
    pub kind: AuthKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Form,
    Url,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInteraction {
    pub request_id: String,
    pub mode: AuthMode,
    pub message: String,
    pub requested_schema: Option<Value>,
    pub url: Option<String>,
    pub elicitation_id: Option<String>,
}
impl std::fmt::Debug for AuthInteraction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Even public display fields and URLs may contain authorization codes.
        f.debug_struct("AuthInteraction")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthAction {
    Accept,
    Decline,
    Cancel,
}
/// Not Debug: content may contain credentials. Request identity is mandatory.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResponse {
    pub request_id: String,
    pub action: AuthAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Map<String, Value>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResult {
    pub authenticated: bool,
    pub error: Option<String>,
}
#[derive(Debug, Clone)]
pub enum AuthEvent {
    Ready(AuthDescriptor),
    Interaction(AuthInteraction),
    Result(AuthResult),
}
/// Errors never include native output, environment, response content, or OS error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    Spawn,
    Io,
    Protocol,
    Limit,
    Timeout,
    Identity,
    Closed,
    Exit,
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "authentication operation failed ({self:?})")
    }
}
impl std::error::Error for AuthError {}

struct ChildOwner {
    child: Box<dyn TokioChildWrapper>,
    stderr: JoinHandle<()>,
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.stderr.abort();
    }
}
fn spawn(options: &AuthOptions, describe: bool) -> Result<ChildOwner, AuthError> {
    let mut command = crate::process::background_command(&options.executable);
    command.args([
        "auth",
        if describe { "describe" } else { "login" },
        "--provider",
        &options.provider,
        if describe { "--json" } else { "--rpc" },
    ]);
    command
        .envs(&options.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    let mut command = TokioCommandWrap::from(command);
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::JobObject);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    command.wrap(process_wrap::tokio::KillOnDrop);
    let mut child = command.spawn().map_err(|_| AuthError::Spawn)?;
    let mut stderr = child.stderr().take().ok_or(AuthError::Io)?;
    // Drain indefinitely with fixed storage; never retain or display secret-bearing stderr.
    let stderr = tokio::spawn(async move {
        let mut bytes = [0; LIMIT];
        loop {
            match stderr.read(&mut bytes).await {
                Ok(0) | Err(_) => break,
                _ => {}
            }
        }
    });
    Ok(ChildOwner { child, stderr })
}
fn descriptor(value: Value, provider: &str) -> Result<AuthDescriptor, AuthError> {
    let d: AuthDescriptor = serde_json::from_value(value).map_err(|_| AuthError::Protocol)?;
    if !d.rpc || d.provider != provider {
        return Err(AuthError::Identity);
    }
    Ok(d)
}
/// Side-effect-free capability probe. Output (including JSON whitespace) is capped at 64 KiB.
pub async fn describe(options: &AuthOptions) -> Result<AuthDescriptor, AuthError> {
    let mut owner = spawn(options, true)?;
    owner.child.stdin().take();
    let stdout = owner.child.stdout().take().ok_or(AuthError::Io)?;
    let result = timeout_at(Instant::now() + DESCRIBE, async {
        let mut bytes = Vec::new();
        stdout
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| AuthError::Io)?;
        if bytes.len() > LIMIT {
            return Err(AuthError::Limit);
        }
        let status = Box::into_pin(owner.child.wait())
            .await
            .map_err(|_| AuthError::Io)?;
        if !status.success() {
            return Err(AuthError::Exit);
        }
        descriptor(
            serde_json::from_slice(&bytes).map_err(|_| AuthError::Protocol)?,
            &options.provider,
        )
    })
    .await
    .map_err(|_| AuthError::Timeout)?;
    let _ = owner.child.start_kill();
    let _ = Box::into_pin(owner.child.wait()).await;
    result
}
async fn frame(reader: &mut BufReader<ChildStdout>) -> Result<Value, AuthError> {
    let mut bytes = Vec::new();
    loop {
        let part = reader.fill_buf().await.map_err(|_| AuthError::Io)?;
        if part.is_empty() {
            return Err(AuthError::Protocol);
        }
        let end = part.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let count = end.unwrap_or(part.len());
        if bytes.len() + count > LIMIT {
            return Err(AuthError::Limit);
        }
        bytes.extend_from_slice(&part[..count]);
        reader.consume(count);
        if end.is_some() {
            return serde_json::from_slice(&bytes).map_err(|_| AuthError::Protocol);
        }
    }
}
type ResponseJob = (AuthResponse, oneshot::Sender<Result<(), AuthError>>);
/// Owns the worker, pipes, child and process tree. Drop cancels the worker and kills
/// the tree; `shutdown` additionally waits for the child to be reaped. No global state.
pub struct AuthProcess {
    events: mpsc::Receiver<Result<AuthEvent, AuthError>>,
    responses: mpsc::Sender<ResponseJob>,
    answered: tokio::sync::Mutex<HashSet<String>>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for AuthProcess {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}
impl AuthProcess {
    pub fn spawn(options: AuthOptions) -> Result<Self, AuthError> {
        let owner = spawn(&options, false)?;
        let (events_tx, events) = mpsc::channel(4);
        let (responses, responses_rx) = mpsc::channel(1);
        let (stop, stop_rx) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let mut owner = owner;
            let deadline = Instant::now() + OVERALL;
            let result = tokio::select! {
                _ = stop_rx => Ok(()),
                result = timeout_at(deadline, run(&mut owner, &options, &events_tx, responses_rx, deadline)) => result.unwrap_or(Err(AuthError::Timeout)),
            };
            let _ = owner.child.start_kill();
            let _ = Box::into_pin(owner.child.wait()).await;
            if let Err(error) = result {
                let _ = events_tx.send(Err(error)).await;
            }
        });
        Ok(Self {
            events,
            responses,
            answered: tokio::sync::Mutex::new(HashSet::new()),
            stop: Some(stop),
            worker: Some(worker),
        })
    }
    pub async fn next_event(&mut self) -> Result<Option<AuthEvent>, AuthError> {
        self.events.recv().await.transpose()
    }
    pub async fn respond(&self, response: AuthResponse) -> Result<(), AuthError> {
        let mut answered = self.answered.lock().await;
        if answered.contains(&response.request_id) {
            return Err(AuthError::Identity);
        }
        let id = response.request_id.clone();
        let (tx, rx) = oneshot::channel();
        self.responses
            .send((response, tx))
            .await
            .map_err(|_| AuthError::Closed)?;
        let result = rx.await.map_err(|_| AuthError::Closed)?;
        if result.is_ok() {
            answered.insert(id);
        }
        result
    }
    pub async fn shutdown(&mut self) -> Result<(), AuthError> {
        // Unblock terminal error delivery even if the adapter stopped draining events.
        self.events.close();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let result = match &mut self.worker {
            Some(worker) => worker.await.map_err(|_| AuthError::Io),
            None => return Ok(()),
        };
        self.worker.take();
        result
    }
}
async fn emit(
    tx: &mpsc::Sender<Result<AuthEvent, AuthError>>,
    event: AuthEvent,
) -> Result<(), AuthError> {
    tx.send(Ok(event)).await.map_err(|_| AuthError::Closed)
}
async fn write_response<W: tokio::io::AsyncWrite + Unpin>(
    stdin: &mut W,
    bytes: &[u8],
) -> Result<(), AuthError> {
    stdin.write_all(bytes).await.map_err(|_| AuthError::Io)?;
    stdin.flush().await.map_err(|_| AuthError::Io)
}

async fn frame_or_response(
    stdout: &mut BufReader<ChildStdout>,
    responses: &mut mpsc::Receiver<ResponseJob>,
) -> Result<Value, AuthError> {
    let read = frame(stdout);
    tokio::pin!(read);
    loop {
        tokio::select! {
            value = &mut read => return value,
            job = responses.recv() => {
                let (_, ack) = job.ok_or(AuthError::Closed)?;
                let _ = ack.send(Err(AuthError::Identity));
            }
        }
    }
}
async fn run(
    owner: &mut ChildOwner,
    options: &AuthOptions,
    events: &mpsc::Sender<Result<AuthEvent, AuthError>>,
    mut responses: mpsc::Receiver<ResponseJob>,
    deadline: Instant,
) -> Result<(), AuthError> {
    let mut stdout = BufReader::new(owner.child.stdout().take().ok_or(AuthError::Io)?);
    let mut stdin = owner.child.stdin().take().ok_or(AuthError::Io)?;
    // auth_ready intentionally has no `rpc`: only `auth describe --json` does.
    #[derive(Deserialize)]
    struct Ready {
        provider: String,
        kind: AuthKind,
    }
    let ready = frame_or_response(&mut stdout, &mut responses).await?;
    if ready["type"] != "auth_ready" {
        return Err(AuthError::Protocol);
    }
    let ready: Ready = serde_json::from_value(ready).map_err(|_| AuthError::Protocol)?;
    if ready.provider != options.provider {
        return Err(AuthError::Identity);
    }
    emit(
        events,
        AuthEvent::Ready(AuthDescriptor {
            rpc: true,
            provider: ready.provider,
            kind: ready.kind,
        }),
    )
    .await?;
    let mut seen = HashSet::new();
    loop {
        // Frame reads are never cancelled by responses: partially read frames stay intact.
        // Responses outside an interaction are rejected rather than queued for a later prompt.
        let value = frame_or_response(&mut stdout, &mut responses).await?;
        match value["type"].as_str() {
            Some("auth_interaction") => {
                let interaction: AuthInteraction =
                    serde_json::from_value(value).map_err(|_| AuthError::Protocol)?;
                if interaction.request_id.is_empty()
                    || !seen.insert(interaction.request_id.clone())
                    || seen.len() > 1024
                {
                    return Err(AuthError::Protocol);
                }
                if interaction.mode == AuthMode::Url
                    && interaction.url.as_deref().unwrap_or("").is_empty()
                {
                    return Err(AuthError::Protocol);
                }
                let id = interaction.request_id.clone();
                let prompt_deadline = deadline.min(Instant::now() + options.prompt_timeout);
                timeout_at(
                    prompt_deadline,
                    emit(events, AuthEvent::Interaction(interaction)),
                )
                .await
                .map_err(|_| AuthError::Timeout)??;
                loop {
                    let (response, ack) = timeout_at(prompt_deadline, responses.recv())
                        .await
                        .map_err(|_| AuthError::Timeout)?
                        .ok_or(AuthError::Closed)?;
                    if response.request_id != id {
                        let _ = ack.send(Err(AuthError::Identity));
                        continue;
                    }
                    let result = timeout_at(prompt_deadline, async {
                        let mut value =
                            serde_json::to_value(response).map_err(|_| AuthError::Protocol)?;
                        value["type"] = "auth_response".into();
                        let mut bytes =
                            serde_json::to_vec(&value).map_err(|_| AuthError::Protocol)?;
                        if bytes.len() >= LIMIT {
                            return Err(AuthError::Limit);
                        }
                        bytes.push(b'\n');
                        write_response(&mut stdin, &bytes).await
                    })
                    .await
                    .unwrap_or(Err(AuthError::Timeout));
                    let _ = ack.send(result);
                    result?;
                    break;
                }
            }
            Some("auth_result") => {
                let authenticated = value["authenticated"]
                    .as_bool()
                    .ok_or(AuthError::Protocol)?;
                drop(stdin);
                let status = Box::into_pin(owner.child.wait())
                    .await
                    .map_err(|_| AuthError::Io)?;
                if status.code().is_none() || authenticated != status.success() {
                    return Err(AuthError::Exit);
                }
                // Native error strings can contain credentials. Expose only a fixed reason.
                return emit(
                    events,
                    AuthEvent::Result(AuthResult {
                        authenticated,
                        error: if authenticated {
                            None
                        } else {
                            Some("authentication was not completed".into())
                        },
                    }),
                )
                .await;
            }
            _ => return Err(AuthError::Protocol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocked_response_write_expires() {
        let (mut stdin, _unread) = tokio::io::duplex(1);
        assert_eq!(
            timeout_at(
                Instant::now() + Duration::from_millis(30),
                write_response(&mut stdin, b"response\n"),
            )
            .await
            .unwrap_or(Err(AuthError::Timeout)),
            Err(AuthError::Timeout),
        );
    }
}
