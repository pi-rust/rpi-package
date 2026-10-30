//! Connection-owned reverse requests. No worker outlives its owner.
use crate::sampling::*;
use crate::Control;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

pub(crate) type Reply = Arc<dyn Fn(Value) + Send + Sync>;

/// Sticky, connection/exchange-owned failure, independent of reply queue capacity.
/// Control's bounded polling also wakes an async owner with a stalled input stream.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReplyFailure(Control);
impl ReplyFailure {
    pub(crate) fn fail(&self) { self.0.cancel(); }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.0.check().map_err(|_| "MCP callback reply transport failed (queue full or closed)".into())
    }
    pub(crate) async fn failed(&self) { self.0.cancelled().await; }
}
struct Job {
    id: Value,
    context: SamplingContext,
    request: SamplingRequest,
    host: Control,
    cancel: Control,
    reply: Reply,
}
#[derive(Default)]
struct State {
    active: Option<(SamplingContext, Control)>,
    // None is a completed tombstone. Never evict within a connection generation.
    ids: HashMap<String, Option<Control>>,
    running: usize,
}
pub(crate) struct Callbacks {
    state: Arc<Mutex<State>>,
    jobs: Mutex<Option<mpsc::SyncSender<Job>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}
impl std::fmt::Debug for Callbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Callbacks([redacted])")
    }
}
fn key(id: &Value) -> Option<String> {
    match id {
        Value::String(s) if s.len() <= 1024 => Some(format!("s:{s}")),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(format!("n:{n}")),
        _ => None,
    }
}
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
impl Callbacks {
    pub(crate) fn new(options: SamplingOptions) -> Result<Self, String> {
        let state = Arc::new(Mutex::new(State::default()));
        let (tx, rx) = mpsc::sync_channel::<Job>(4);
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::new();
        // Build all fallible runtimes before starting any owned thread.
        let runtimes = (0..4).map(|_| tokio::runtime::Builder::new_current_thread().enable_all().build())
            .collect::<Result<Vec<_>, _>>().map_err(|_| "MCP callback runtime unavailable")?;
        for runtime in runtimes {
            let rx = rx.clone();
            let state = state.clone();
            let options = options.clone();
            workers.push(std::thread::spawn(move || {
                loop {
                    let job = match rx.lock().unwrap().recv() { Ok(j) => j, Err(_) => break };
                    let result = runtime.block_on(async {
                        tokio::select! {
                            biased;
                            _ = job.host.cancelled() => Err(SamplingError::Cancelled),
                            _ = job.cancel.cancelled() => Err(SamplingError::Cancelled),
                            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(job.context.deadline)) => Err(SamplingError::TimedOut),
                            result = async {
                                let policy = options.authorizer.authorize(&job.context, &job.request, &job.cancel).await?;
                                execute_validated(Some(options.executor.as_ref()), Some(&policy), &job.context, &job.request, &job.cancel).await
                            } => result,
                        }
                    });
                    let mut state = state.lock().unwrap();
                    state.running -= 1;
                    state.ids.insert(key(&job.id).unwrap(), None);
                    // Retirement is linearized against reply enqueue.
                    if state.active.as_ref().is_some_and(|(c, _)| c == &job.context) {
                        let reply = match result {
                            Ok(r) => json!({"jsonrpc":"2.0","id":job.id,"result":r.as_value()}),
                            Err(e) => error(job.id, e.code(), e.message()),
                        };
                        (job.reply)(reply);
                    }
                }
            }));
        }
        Ok(Self { state, jobs: Mutex::new(Some(tx)), workers: Mutex::new(workers) })
    }
    pub(crate) fn shutdown(&self) {
        self.retire();
        self.jobs.lock().unwrap().take();
        for worker in self.workers.lock().unwrap().drain(..) { let _ = worker.join(); }
    }
    pub(crate) fn activate(&self, context: SamplingContext, host: Control) {
        self.retire();
        self.state.lock().unwrap().active = Some((context, host));
    }
    pub(crate) fn retire(&self) {
        let mut state = self.state.lock().unwrap();
        state.active = None;
        for c in state.ids.values().flatten() { c.cancel(); }
    }
    pub(crate) fn dispatch(&self, value: &Value, reply: Reply) {
        if value["method"] == "notifications/cancelled" && value.get("id").is_none() {
            if let Some(k) = key(&value["params"]["requestId"]) {
                if let Some(Some(c)) = self.state.lock().unwrap().ids.get(&k) { c.cancel(); }
            }
            return;
        }
        let Some(id) = value.get("id") else { return };
        let Some(k) = key(id) else {
            reply(error(Value::Null, -32600, "Invalid request ID")); return;
        };
        if value["method"] != "sampling/createMessage" {
            if let Some(r) = crate::transport::client_reply(value) { reply(r); }
            return;
        }
        // Validate borrowed data before cloning any remote payload into a job.
        if let Err(e) = crate::sampling::validate_request(&value["params"]) {
            reply(error(id.clone(), e.code(), e.message())); return;
        }
        let request = match SamplingRequest::parse(value["params"].clone()) {
            Ok(r) => r,
            Err(e) => { reply(error(id.clone(), e.code(), e.message())); return; }
        };
        let mut state = self.state.lock().unwrap();
        let Some((context, host)) = state.active.clone() else {
            reply(error(id.clone(), -32603, "Sampling denied")); return;
        };
        if state.ids.contains_key(&k) {
            reply(error(id.clone(), -32600, "Duplicate callback ID")); return;
        }
        if state.running >= 4 || state.ids.len() >= 1024 {
            reply(error(id.clone(), -32603, "Sampling capacity exceeded")); return;
        }
        let cancel = Control::default();
        state.ids.insert(k.clone(), Some(cancel.clone()));
        state.running += 1;
        let job = Job { id: id.clone(), context, request, host, cancel, reply: reply.clone() };
        if self.jobs.lock().unwrap().as_ref().map_or(true, |tx| tx.try_send(job).is_err()) {
            state.running -= 1;
            state.ids.insert(k, None);
            reply(error(id.clone(), -32603, "Sampling capacity exceeded"));
        }
    }
}
impl Drop for Callbacks {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    struct Pending;
    impl SamplingExecutor for Pending {
        fn execute<'a>(&'a self, _: &'a SamplingContext, _: &'a SamplingRequest, _: &'a Control) -> SamplingFuture<'a> {
            Box::pin(std::future::pending())
        }
    }
    impl SamplingAuthorizer for Pending {
        fn authorize<'a>(&'a self, c: &'a SamplingContext, _: &'a SamplingRequest, _: &'a Control) -> AuthorizationFuture<'a> {
            Box::pin(async move { Ok(SamplingPolicy {context:c.clone(),max_messages:128,max_tokens:16384,max_duration:Duration::from_secs(300)}) })
        }
    }
    fn owner() -> Callbacks { Callbacks::new(SamplingOptions {executor:Arc::new(Pending),authorizer:Arc::new(Pending)}).unwrap() }
    fn context() -> SamplingContext { SamplingContext {namespace:"random-runtime".into(),scope:"scope-a".into(),server:"server".into(),cwd:std::env::current_dir().unwrap(),target:"canonical-target".into(),generation:1,operation:"tools/call:3".into(),deadline:Instant::now()+Duration::from_secs(3)} }
    fn request(id:Value) -> Value { json!({"jsonrpc":"2.0","id":id,"method":"sampling/createMessage","params":{"messages":[{"role":"user","content":{"type":"text","text":"private"}}],"maxTokens":1}}) }
    fn output() -> (Reply,mpsc::Receiver<Value>) { let (tx,rx)=mpsc::sync_channel(32); (Arc::new(move |v| {let _=tx.try_send(v);}),rx) }
    #[test]
    fn bounded_workers_duplicate_live_id_and_ping() {
        let c=owner(); c.activate(context(),Control::default()); let (reply,rx)=output();
        for id in 0..4 { c.dispatch(&request(json!(id)),reply.clone()); }
        c.dispatch(&request(json!(0)),reply.clone());
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap()["error"]["code"],-32600);
        assert!(c.state.lock().unwrap().ids["n:0"].is_some());
        c.dispatch(&request(json!(4)),reply.clone());
        assert_eq!(rx.recv().unwrap()["error"]["message"],"Sampling capacity exceeded");
        c.dispatch(&json!({"jsonrpc":"2.0","id":"p","method":"ping"}),reply);
        assert_eq!(rx.recv().unwrap()["result"],json!({})); c.shutdown();
    }
    #[test]
    fn exact_typed_cancel_and_connection_isolation() {
        let a=owner(); let b=owner(); a.activate(context(),Control::default()); b.activate(context(),Control::default());
        let (reply,rx)=output(); a.dispatch(&request(json!(7)),reply.clone()); b.dispatch(&request(json!(7)),reply.clone());
        a.dispatch(&json!({"method":"notifications/cancelled","params":{"requestId":"7"}}),reply.clone());
        assert!(rx.recv_timeout(Duration::from_millis(40)).is_err());
        a.dispatch(&json!({"method":"notifications/cancelled","params":{"requestId":7}}),reply);
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap()["error"]["message"],"Sampling cancelled");
        assert!(b.state.lock().unwrap().ids["n:7"].as_ref().unwrap().check().is_ok());
        a.shutdown(); b.shutdown();
    }
    #[test]
    fn idle_retirement_tombstones_and_generation_do_not_leak() {
        let c=owner(); let (reply,rx)=output(); c.dispatch(&request(json!("idle")),reply.clone());
        assert_eq!(rx.recv().unwrap()["error"]["message"],"Sampling denied");
        c.activate(context(),Control::default()); c.dispatch(&request(json!("old")),reply.clone()); c.retire();
        let mut next=context(); next.generation=2; next.scope="sibling".into(); c.activate(next,Control::default());
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        c.dispatch(&request(json!("old")),reply);
        assert_eq!(rx.recv().unwrap()["error"]["message"],"Duplicate callback ID"); c.shutdown();
    }
    #[test]
    fn malformed_and_unknown_methods_are_fixed_errors() {
        let c=owner(); let (reply,rx)=output();
        for id in [Value::Null,json!(1.5),json!([]),json!({})] {
            c.dispatch(&request(id),reply.clone()); let v=rx.recv().unwrap(); assert!(v["id"].is_null()); assert_eq!(v["error"]["code"],-32600);
        }
        c.dispatch(&json!({"method":"private-unknown","id":-9}),reply);
        let v=rx.recv().unwrap(); assert_eq!(v["id"],-9); assert_eq!(v["error"]["code"],-32601); assert!(!v.to_string().contains("private"));
    }
}
