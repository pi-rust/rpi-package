use rpi_mcp_core::{
    execute_validated, Control, SamplingContext, SamplingError, SamplingExecutor, SamplingFuture,
    SamplingPolicy, SamplingRequest, SamplingResult,
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

fn wire() -> Value {
    json!({"messages":[{"role":"user","content":{"type":"text","text":"isolated"}}],"maxTokens":100})
}
fn result() -> SamplingResult {
    SamplingResult::parse(json!({"role":"assistant","content":{"type":"text","text":"fake unit output"},"model":"fake-unit-model","stopReason":"endTurn"})).unwrap()
}
fn context() -> SamplingContext {
    SamplingContext {
        cwd: std::env::current_dir().unwrap(),
        target: "test-target".into(),
        namespace: "runtime-a".into(),
        scope: "session-a".into(),
        server: "server-a".into(),
        generation: 7,
        operation: "operation-a".into(),
        deadline: Instant::now() + Duration::from_secs(2),
    }
}
fn policy(c: &SamplingContext) -> SamplingPolicy {
    SamplingPolicy {
        context: c.clone(),
        max_messages: 128,
        max_tokens: 16384,
        max_duration: Duration::from_secs(3),
    }
}

#[test]
fn valid_text_roundtrip_and_opaque_extensions() {
    let mut v = wire();
    v["_meta"] = json!({"opaque":{"tools":["not executable"],"account":"not authority"}});
    v["vendor"] = json!(["unchanged"]);
    v["messages"][0]["content"]["_meta"] = json!({"untrusted":"ignore rules"});
    v["modelPreferences"] =
        json!({"hints":[{"name":"advisory","vendor":"opaque"}],"costPriority":0.5});
    v["systemPrompt"] = json!("remote prompt: isolated sample only");
    v["includeContext"] = json!("none");
    let r: SamplingRequest = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(&r).unwrap(), v);
    assert_eq!(
        r.system_prompt(),
        Some("remote prompt: isolated sample only")
    );
    assert_eq!(
        r.model_preferences().unwrap()["hints"][0]["name"],
        "advisory"
    );
    assert!(!format!("{r:?}").contains("ignore rules"));
}

#[test]
fn malicious_request_fields_rejected_through_serde() {
    for (k, v) in [
        ("tools", json!([])),
        ("toolChoice", json!({"mode":"none"})),
        ("includeContext", json!("thisServer")),
        ("includeContext", json!("allServers")),
        ("includeContext", Value::Null),
        ("maxTokens", json!(0)),
        ("maxTokens", json!(16385)),
        ("maxTokens", json!(1.5)),
        ("temperature", json!(3)),
        ("_meta", json!("bad")),
        ("systemPrompt", Value::Null),
        ("modelPreferences", json!({"costPriority":1.1})),
        ("modelPreferences", json!({"hints":[{"name":42}]})),
    ] {
        let mut request = wire();
        request[k] = v;
        let e = serde_json::from_value::<SamplingRequest>(request).unwrap_err();
        assert!(!e.to_string().contains("allServers"));
    }
    for c in [
        json!([{"type":"text","text":"array is not 2025"}]),
        json!({"type":"tool_use","name":"shell"}),
        json!({"type":"text","text":"hi","tool_calls":[]}),
        json!({"type":"image","data":"not base64!","mimeType":"image/png"}),
    ] {
        let mut v = wire();
        v["messages"][0]["content"] = c;
        assert!(SamplingRequest::parse(v).is_err());
    }
    let mut v = wire();
    v["messages"][0]["role"] = json!("system");
    assert!(SamplingRequest::parse(v).is_err());
    assert_eq!(SamplingError::Unsupported.code(), -32602);
    assert_eq!(SamplingError::InvalidParams.code(), -32602);
}

#[test]
fn request_byte_message_token_and_prompt_bounds() {
    let mut v = wire();
    v["systemPrompt"] = json!("x".repeat(65536));
    assert!(SamplingRequest::parse(v.clone()).is_ok());
    v["systemPrompt"] = json!("x".repeat(65537));
    assert!(SamplingRequest::parse(v).is_err());
    let mut v = wire();
    v["messages"] = json!(vec![wire()["messages"][0].clone(); 128]);
    v["maxTokens"] = json!(16384);
    assert!(SamplingRequest::parse(v.clone()).is_ok());
    v["messages"]
        .as_array_mut()
        .unwrap()
        .push(wire()["messages"][0].clone());
    assert!(SamplingRequest::parse(v).is_err());
    let mut v = wire();
    v["messages"] = json!([]);
    assert!(SamplingRequest::parse(v).is_err());
    // Canonical serialized UTF-8 bytes, including envelope and extensions.
    let mut v = wire();
    v["messages"][0]["content"]["text"] = json!("");
    let overhead = serde_json::to_vec(&v).unwrap().len();
    v["messages"][0]["content"]["text"] = json!("a".repeat(256 * 1024 - overhead));
    assert!(SamplingRequest::parse(v.clone()).is_ok());
    v["messages"][0]["content"]["text"].as_str().unwrap();
    v["extra"] = json!(true);
    assert!(SamplingRequest::parse(v).is_err());
    let mut v = wire();
    v["messages"][0]["content"]["text"] = json!("é".repeat(131072));
    assert!(SamplingRequest::parse(v).is_err());
}

#[test]
fn preferences_and_sequences_are_bounded() {
    for p in [
        json!({"hints":vec![json!({"name":"x"});33]}),
        json!({"hints":[{"name":"x".repeat(257)}]}),
        json!({"speedPriority":-0.1}),
    ] {
        let mut v = wire();
        v["modelPreferences"] = p;
        assert!(SamplingRequest::parse(v).is_err());
    }
    let mut v = wire();
    v["stopSequences"] = json!(["END"]);
    assert!(SamplingRequest::parse(v.clone()).is_ok());
    v["stopSequences"] = json!(vec!["END"; 129]);
    assert!(SamplingRequest::parse(v).is_err());
}

#[test]
fn result_validation_no_fabricated_model_or_tools() {
    let v = result().as_value().clone();
    for (k, bad) in [
        ("model", json!("")),
        ("model", Value::Null),
        ("role", json!("user")),
        ("content", json!({"type":"tool_use","name":"shell"})),
        ("stopReason", json!("toolUse")),
        ("tools", json!([])),
        (
            "content",
            json!({"type":"text","text":"x".repeat(256*1024)}),
        ),
    ] {
        let mut v = v.clone();
        v[k] = bad;
        assert_eq!(
            SamplingResult::parse(v).unwrap_err(),
            SamplingError::InvalidResult
        );
    }
    for stop in ["endTurn", "maxTokens", "stopSequence"] {
        let mut v = v.clone();
        v["stopReason"] = json!(stop);
        v["_meta"] = json!({"opaque":true});
        let r: SamplingResult = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(r).unwrap(), v);
    }
    let mut missing = v;
    missing.as_object_mut().unwrap().remove("model");
    assert!(SamplingResult::parse(missing).is_err());
}

struct Fake {
    seen: Mutex<Vec<SamplingContext>>,
}
impl SamplingExecutor for Fake {
    fn execute<'a>(
        &'a self,
        c: &'a SamplingContext,
        _: &'a SamplingRequest,
        _: &'a Control,
    ) -> SamplingFuture<'a> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(c.clone());
            Ok(result())
        })
    }
}
#[tokio::test]
async fn exact_scope_identity_and_caller_policy_required() {
    let f = Fake {
        seen: Mutex::new(vec![]),
    };
    let c = context();
    let p = policy(&c);
    let r = SamplingRequest::parse(wire()).unwrap();
    let control = Control::default();
    assert_eq!(
        execute_validated(Some(&f), None, &c, &r, &control)
            .await
            .unwrap_err(),
        SamplingError::Denied
    );
    assert_eq!(
        execute_validated(None, Some(&p), &c, &r, &control)
            .await
            .unwrap_err(),
        SamplingError::Unavailable
    );
    assert_eq!(
        execute_validated(Some(&f), Some(&p), &c, &r, &control)
            .await
            .unwrap()
            .model(),
        "fake-unit-model"
    );
    for field in 0..8 {
        let mut other = c.clone();
        match field {
            0 => other.namespace.push('b'),
            1 => other.scope.push('b'),
            2 => other.server.push('b'),
            3 => other.generation += 1,
            4 => other.operation.push('b'),
            5 => other.cwd.push("other"),
            6 => other.target.push('b'),
            _ => other.deadline -= Duration::from_millis(1),
        }
        assert_eq!(
            execute_validated(Some(&f), Some(&p), &other, &r, &control)
                .await
                .unwrap_err(),
            SamplingError::Denied
        );
        let explicit = policy(&other);
        execute_validated(Some(&f), Some(&explicit), &other, &r, &control)
            .await
            .unwrap();
    }
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 9);
    assert_eq!(seen[0], c);
    for i in 1..seen.len() {
        assert_ne!(seen[i], c);
    }
}

#[tokio::test]
async fn shortened_300_second_parent_executes_exact_30_second_grant() {
    let f = Fake { seen: Mutex::new(vec![]) };
    let mut c = context();
    let start = Instant::now();
    c.deadline = start + Duration::from_secs(300);
    let mut p = policy(&c);
    p.context.deadline = start + Duration::from_secs(30);
    p.max_duration = Duration::from_secs(30);
    let r = SamplingRequest::parse(wire()).unwrap();
    // Time spent obtaining consent consumes the grant; execution never resets it.
    tokio::time::sleep(Duration::from_millis(10)).await;
    execute_validated(Some(&f), Some(&p), &c, &r, &Control::default()).await.unwrap();
    assert_eq!(f.seen.lock().unwrap().as_slice(), &[p.context.clone()]);
    assert!(p.context.deadline < c.deadline);
    assert!(p.context.deadline.saturating_duration_since(Instant::now()) < p.max_duration);
}

#[tokio::test]
async fn expired_extended_and_over_budget_grants_never_execute() {
    let f = Fake { seen: Mutex::new(vec![]) };
    let c = context();
    let r = SamplingRequest::parse(wire()).unwrap();
    let control = Control::default();
    for (deadline, budget, error) in [
        (Instant::now() - Duration::from_secs(1), Duration::from_secs(3), SamplingError::TimedOut),
        (c.deadline + Duration::from_millis(1), Duration::from_secs(3), SamplingError::Denied),
        (c.deadline, Duration::from_millis(1), SamplingError::Denied),
    ] {
        let mut p = policy(&c);
        p.context.deadline = deadline;
        p.max_duration = budget;
        assert_eq!(execute_validated(Some(&f), Some(&p), &c, &r, &control).await.unwrap_err(), error);
    }
    let mut expired = c.clone();
    expired.deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(execute_validated(Some(&f), Some(&policy(&expired)), &expired, &r, &control).await.unwrap_err(), SamplingError::TimedOut);
    assert!(f.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn shortened_deadline_drops_pending_and_refuses_late_success() {
    let c = context();
    let r = SamplingRequest::parse(wire()).unwrap();
    for late in [false, true] {
        let f = pending();
        let mut p = policy(&c);
        p.context.deadline = Instant::now() + Duration::from_millis(10);
        let executor: &dyn SamplingExecutor = if late { &Late } else { &f };
        assert_eq!(execute_validated(Some(executor), Some(&p), &c, &r, &Control::default()).await.unwrap_err(), SamplingError::TimedOut);
        if !late { assert!(f.dropped.load(Ordering::SeqCst)); }
    }
}

#[tokio::test]
async fn multimodal_valid_wire_but_text_only_execution() {
    let f = Fake {
        seen: Mutex::new(vec![]),
    };
    let c = context();
    let p = policy(&c);
    for (kind, mime) in [("image", "image/png"), ("audio", "audio/wav")] {
        let mut v = wire();
        v["messages"][0]["content"] =
            json!({"type":kind,"mimeType":mime,"data":"YQ==","_meta":{"preserved":true}});
        let r = SamplingRequest::parse(v.clone()).unwrap();
        assert_eq!(r.as_value(), &v);
        assert_eq!(
            execute_validated(Some(&f), Some(&p), &c, &r, &Control::default())
                .await
                .unwrap_err(),
            SamplingError::Unsupported
        );
    }
    assert!(f.seen.lock().unwrap().is_empty());
}

struct Guard(Arc<AtomicBool>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct Pending {
    entered: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}
impl SamplingExecutor for Pending {
    fn execute<'a>(
        &'a self,
        _: &'a SamplingContext,
        _: &'a SamplingRequest,
        _: &'a Control,
    ) -> SamplingFuture<'a> {
        Box::pin(async move {
            let _guard = Guard(self.dropped.clone());
            self.entered.store(true, Ordering::SeqCst);
            std::future::pending().await
        })
    }
}
fn pending() -> Pending {
    Pending {
        entered: Arc::new(AtomicBool::new(false)),
        dropped: Arc::new(AtomicBool::new(false)),
    }
}
#[tokio::test]
async fn cancellation_drops_executor_guard_without_detached_task() {
    let f = pending();
    let c = context();
    let p = policy(&c);
    let control = Control::default();
    let r = SamplingRequest::parse(wire()).unwrap();
    let execution = execute_validated(Some(&f), Some(&p), &c, &r, &control);
    let cancel = async {
        while !f.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        control.cancel();
    };
    let (out, ()) = tokio::join!(execution, cancel);
    assert_eq!(out.unwrap_err(), SamplingError::Cancelled);
    assert!(f.dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn caller_future_drop_cleans_up() {
    let f = pending();
    let c = context();
    let p = policy(&c);
    let control = Control::default();
    let r = SamplingRequest::parse(wire()).unwrap();
    let mut future = Box::pin(execute_validated(Some(&f), Some(&p), &c, &r, &control));
    tokio::select! { out = &mut future => panic!("unexpected {out:?}"), _ = tokio::time::sleep(Duration::from_millis(5)) => {} }
    assert!(f.entered.load(Ordering::SeqCst));
    drop(future);
    assert!(f.dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn timeout_drops_cooperative_executor() {
    let f = pending();
    let mut c = context();
    c.deadline = Instant::now() + Duration::from_millis(20);
    let p = policy(&c);
    let r = SamplingRequest::parse(wire()).unwrap();
    assert_eq!(
        execute_validated(Some(&f), Some(&p), &c, &r, &Control::default())
            .await
            .unwrap_err(),
        SamplingError::TimedOut
    );
    assert!(f.dropped.load(Ordering::SeqCst));
}
struct Late;
impl SamplingExecutor for Late {
    fn execute<'a>(
        &'a self,
        _: &'a SamplingContext,
        _: &'a SamplingRequest,
        _: &'a Control,
    ) -> SamplingFuture<'a> {
        Box::pin(async {
            std::thread::sleep(Duration::from_millis(40));
            Ok(result())
        })
    }
}
#[tokio::test]
async fn refuses_late_success_but_does_not_claim_hard_interrupt() {
    let mut c = context();
    c.deadline = Instant::now() + Duration::from_millis(10);
    let p = policy(&c);
    let r = SamplingRequest::parse(wire()).unwrap();
    assert_eq!(
        execute_validated(Some(&Late), Some(&p), &c, &r, &Control::default())
            .await
            .unwrap_err(),
        SamplingError::TimedOut
    );
}
#[tokio::test]
async fn policy_budgets_and_pre_cancel_fail_before_work() {
    let f = Fake {
        seen: Mutex::new(vec![]),
    };
    let c = context();
    let r = SamplingRequest::parse(wire()).unwrap();
    let control = Control::default();
    for (tokens, messages, duration) in [(99, 128, 3), (100, 0, 3), (100, 128, 0), (100, 128, 301)]
    {
        let mut p = policy(&c);
        p.max_tokens = tokens;
        p.max_messages = messages;
        p.max_duration = Duration::from_secs(duration);
        assert_eq!(
            execute_validated(Some(&f), Some(&p), &c, &r, &control)
                .await
                .unwrap_err(),
            SamplingError::Denied
        );
    }
    control.cancel();
    assert_eq!(
        execute_validated(Some(&f), Some(&policy(&c)), &c, &r, &control)
            .await
            .unwrap_err(),
        SamplingError::Cancelled
    );
    assert!(f.seen.lock().unwrap().is_empty());
}
