//! Bounded, cancellable stdio transport. All pipe operations run on owned workers.
use crate::control::{Control, Deadline, MAX_MESSAGE};
use crate::jsonrpc::{Notification, Request, Response};
use crate::process::ProcessTree;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{
    mpsc::{self, Receiver, SyncSender},
    Arc, Mutex,
};
use std::thread::JoinHandle;

pub use crate::http::http_request;

type WriteJob = (String, SyncSender<Result<(), String>>);
#[derive(Debug)]
pub struct StdioTransport {
    child: ProcessTree,
    writer: Option<SyncSender<WriteJob>>,
    stdout: Option<Receiver<Result<String, String>>>,
    workers: Vec<JoinHandle<()>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    timeout: u64,
}

fn receive<T>(receiver: &Receiver<T>, deadline: &Deadline) -> Result<T, String> {
    loop {
        match receiver.recv_timeout(deadline.slice()?) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("MCP server closed pipe".into())
            }
        }
    }
}

/// Read without allocating an unbounded line, even if the server omits a newline.
fn bounded_line(reader: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    loop {
        let part = reader
            .fill_buf()
            .map_err(|e| format!("read MCP stdout: {e}"))?;
        if part.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let end = part.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let count = end.unwrap_or(part.len());
        if bytes.len() + count > MAX_MESSAGE {
            return Err("MCP response exceeded the 16 MiB limit".into());
        }
        bytes.extend_from_slice(&part[..count]);
        reader.consume(count);
        if end.is_some() {
            break;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| format!("MCP response is not UTF-8: {e}"))
}

impl StdioTransport {
    pub fn start(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&str>,
        notifications: Option<SyncSender<Value>>,
    ) -> Result<Self, String> {
        let mut builder = Command::new(command);
        builder.args(args).envs(env);
        if let Some(cwd) = cwd {
            builder.current_dir(cwd);
        }
        builder
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Spawn returns an RAII tree owner before any fallible pipe setup.
        let mut child = ProcessTree::spawn(&mut builder)
            .map_err(|e| format!("failed to start MCP stdio server: {e}"))?;
        let mut stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or("MCP stdin unavailable")?;
        let stdout = child
            .child_mut()
            .stdout
            .take()
            .ok_or("MCP stdout unavailable")?;
        let mut stderr_pipe = child
            .child_mut()
            .stderr
            .take()
            .ok_or("MCP stderr unavailable")?;
        let (writes, jobs) = mpsc::sync_channel::<WriteJob>(16);
        let writer = std::thread::spawn(move || {
            while let Ok((line, done)) = jobs.recv() {
                let result = writeln!(stdin, "{line}")
                    .and_then(|_| stdin.flush())
                    .map_err(|e| format!("write MCP stdin: {e}"));
                let failed = result.is_err();
                let _ = done.send(result);
                if failed {
                    break;
                }
            }
        });
        let (sender, receiver) = mpsc::sync_channel(16);
        let replies = writes.clone();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match bounded_line(&mut reader) {
                    Ok(Some(line)) => {
                        // Service server requests even while no host call is polling.
                        // Method messages are either a request (has an id → reply) or a
                        // notification (no id → hand to the dispatcher, which turns
                        // e.g. `resources/updated` into a fresh read).
                        if let Ok(value) = serde_json::from_str::<Value>(&line) {
                            if value.get("method").is_some() && validate_message(&value).is_ok() {
                                match client_reply(&value) {
                                    Some(reply) => {
                                        let (done, ignored) = mpsc::sync_channel(1);
                                        drop(ignored);
                                        if replies.try_send((reply.to_string(), done)).is_err() {
                                            let _ = sender.send(Err(
                                                "MCP server request reply queue full or closed"
                                                    .into(),
                                            ));
                                            break;
                                        }
                                    }
                                    // Never block the reader on the notification queue: a
                                    // full queue drops the notification rather than stalling
                                    // every response behind it.
                                    None => {
                                        if let Some(notifications) = &notifications {
                                            let _ = notifications.try_send(value);
                                        }
                                    }
                                }
                                continue;
                            }
                        }
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let tail = stderr.clone();
        let logger = std::thread::spawn(move || {
            let mut buffer = [0; 2048];
            while let Ok(count) = stderr_pipe.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                if let Ok(mut bytes) = tail.lock() {
                    bytes.extend_from_slice(&buffer[..count]);
                    let excess = bytes.len().saturating_sub(8192);
                    bytes.drain(..excess);
                }
            }
        });
        Ok(Self {
            child,
            writer: Some(writes),
            stdout: Some(receiver),
            workers: vec![writer, reader, logger],
            stderr,
            timeout: 60,
        })
    }

    fn write(&mut self, value: &Value, deadline: &Deadline) -> Result<(), String> {
        deadline.check()?;
        let line = serde_json::to_string(value).map_err(|e| format!("encode MCP message: {e}"))?;
        if line.len() > MAX_MESSAGE {
            return Err("MCP request exceeded the 16 MiB limit".into());
        }
        let (done, reply) = mpsc::sync_channel(1);
        self.writer
            .as_ref()
            .ok_or("MCP connection closed")?
            .try_send((line, done))
            .map_err(|e| format!("queue MCP write: {e}"))?;
        receive(&reply, deadline)?
    }

    pub fn notify(&mut self, notification: &Notification) -> Result<(), String> {
        self.notify_control(notification, &Control::default())
    }
    pub fn notify_control(
        &mut self,
        notification: &Notification,
        control: &Control,
    ) -> Result<(), String> {
        let deadline = Deadline::new(self.timeout, &notification.method, control);
        let result = self.write(
            &serde_json::to_value(notification).map_err(|e| e.to_string())?,
            &deadline,
        );
        self.finish(result)
    }
    pub fn request(&mut self, request: &Request) -> Result<Response, String> {
        self.request_control(request, &Control::default())
    }
    pub fn request_control(
        &mut self,
        request: &Request,
        control: &Control,
    ) -> Result<Response, String> {
        let deadline = Deadline::new(self.timeout, &request.method, control);
        let result = (|| {
            self.write(
                &serde_json::to_value(request).map_err(|e| e.to_string())?,
                &deadline,
            )?;
            loop {
                let line = receive(
                    self.stdout.as_ref().ok_or("MCP connection closed")?,
                    &deadline,
                )??;
                let value: Value = serde_json::from_str(line.trim())
                    .map_err(|e| format!("decode MCP response: {e}"))?;
                validate_message(&value)?;
                if value.get("method").is_some() {
                    if let Some(reply) = client_reply(&value) {
                        self.write(&reply, &deadline)?;
                    }
                    continue;
                }
                if value.get("id").and_then(Value::as_u64) != Some(request.id) {
                    continue;
                }
                return decode_response(value, request.id);
            }
        })();
        self.finish(result)
    }
    fn finish<T>(&mut self, result: Result<T, String>) -> Result<T, String> {
        result.map_err(|error| {
            self.stop();
            let tail = self
                .stderr
                .lock()
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
                .unwrap_or_default();
            if tail.is_empty() {
                error
            } else {
                format!("{error}\nMCP server stderr:\n{tail}")
            }
        })
    }
    pub fn set_timeout(&mut self, seconds: u64) {
        self.timeout = seconds.clamp(1, 300);
    }
    pub fn stop(&mut self) {
        self.writer.take();
        self.stdout.take(); // Release any reader blocked sending into the bounded queue.
                            // Always signal the entire tree, even after its leader has exited:
                            // descendants may still own stdin/stdout/stderr and block worker joins.
        self.child.stop();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn validate_message(value: &Value) -> Result<(), String> {
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("MCP message must use JSON-RPC 2.0".into());
    }
    Ok(())
}
pub(crate) fn decode_response(value: Value, id: u64) -> Result<Response, String> {
    validate_message(&value)?;
    if value.get("id").and_then(Value::as_u64) != Some(id) {
        return Err("MCP response ID does not match request".into());
    }
    if value.get("result").is_some() == value.get("error").is_some() {
        return Err("MCP response must contain exactly one of result or error".into());
    }
    serde_json::from_value(value).map_err(|e| format!("decode MCP response: {e}"))
}
pub(crate) fn client_reply(value: &Value) -> Option<Value> {
    let id = value.get("id")?;
    Some(
        if value.get("method").and_then(Value::as_str) == Some("ping") {
            json!({"jsonrpc":"2.0", "id":id, "result":{}})
        } else {
            json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32601,"message":"Client method not supported"}})
        },
    )
}
