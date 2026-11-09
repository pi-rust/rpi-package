use super::*;
use crate::service::BackendConfig;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncBufReadExt, BufReader};
const SESSION: &str = "session";
const CHILD: &str = "tpchild-11111111-1111-4111-8111-111111111111";
const OTHER: &str = "tpchild-22222222-2222-4222-8222-222222222222";
const PHASE: &str = "tpphase-33333333-3333-4333-8333-333333333333";
const PHASE2: &str = "tpphase-44444444-4444-4444-8444-444444444444";
const BODY: &str = "55555555-5555-4555-8555-555555555555";
fn ready() -> Value {
    json!({"sessionId":SESSION,"toolProgramChildren":profile(),"supportedCommands":[SUMMARY_COMMAND,PHASE_COMMAND,BODY_COMMAND]})
}
fn sq() -> ChildPageQuery {
    ChildPageQuery {
        leaf_id: Some("leaf".into()),
        cursor: None,
        direction: Direction::OldestFirst,
        limit: 32,
        max_bytes: MAX_RESPONSE_BYTES,
    }
}
fn pq() -> ChildPhaseQuery {
    ChildPhaseQuery {
        child_id: CHILD.into(),
        leaf_id: Some("leaf".into()),
        cursor: None,
        direction: Direction::OldestFirst,
        limit: 32,
        max_bytes: MAX_RESPONSE_BYTES,
    }
}
fn bq() -> ChildBodyQuery {
    ChildBodyQuery {
        child_id: CHILD.into(),
        phase_entry_id: PHASE.into(),
        descriptor_id: format!("tpbody-{BODY}-commit"),
        leaf_id: Some("leaf".into()),
        cursor: None,
        limit: 8,
        max_bytes: 65536,
    }
}
fn cursor(child: Option<&str>, entry: &str, seq: u64) -> Value {
    json!({"sessionId":SESSION,"lane":"main","leafId":"leaf","direction":"oldestFirst","childId":child,"afterEntryId":entry,"afterSeq":seq})
}
fn summary_page() -> Value {
    json!({"leafId":"leaf","children":[{"version":1,"childId":CHILD,"seq":10,
        "origin":{"runId":"original-run","assistantEntryId":"original-assistant","toolIndex":2},
        "originVerification":"forkCopy","rootCallId":"call","rootToolName":"program",
        "toolName":"read","ordinal":0,"parentChildId":null,"state":"unknown"}],
        "nextCursor":null,"scanExhausted":true})
}
fn phase_page() -> Value {
    json!({"leafId":"leaf","childId":CHILD,"phases":[{"entryId":PHASE,"seq":20,"version":1,
        "childId":CHILD,"index":4,"phase":"closed","descriptor":null,"isError":null,"cancelled":null,
        "failed":false,"interrupted":true}],"nextCursor":null,"scanned":1,"scanExhausted":true,"state":"unknown"})
}
// Hash the complete body, not just the returned page.
fn body_page(full_body: &[u8], start: usize, count: usize) -> Value {
    let total = full_body.len();
    let digest = format!("{:x}", Sha256::digest(full_body));
    let bytes = &full_body[start * CHUNK_BYTES..((start + count) * CHUNK_BYTES).min(total)];
    let next = start + bytes.len().div_ceil(CHUNK_BYTES);
    let next_cursor = (next < total.div_ceil(CHUNK_BYTES)).then(|| {
        json!({"direction":"oldestFirst","childId":CHILD,
        "phaseEntryId":PHASE,"body":{"sessionId":SESSION,"lane":"main","leafId":"leaf",
        "descriptorId":format!("tpbody-{BODY}-commit"),"nextIndex":next}})
    });
    let chunks: Vec<_> = bytes
        .chunks(CHUNK_BYTES)
        .enumerate()
        .map(|(i, b)| {
            json!({"index":start+i,
        "byteOffset":(start+i)*CHUNK_BYTES,"byteLength":b.len(),"data":STANDARD.encode(b),
        "sha256":format!("{:x}",Sha256::digest(b))})
        })
        .collect();
    json!({"childId":CHILD,"phaseEntryId":PHASE,"body":{"descriptor":{
        "descriptorId":format!("tpbody-{BODY}-commit"),"version":1,"bodyId":BODY,"childId":CHILD,
        "kind":"final","codec":"native-json-v1","encoding":"base64","byteLength":total,
        "chunkBytes":CHUNK_BYTES,"chunks":total.div_ceil(CHUNK_BYTES),"sha256":digest},
        "leafId":"leaf","byteOffset":start*CHUNK_BYTES,"byteLength":bytes.len(),"totalBytes":total,
        "chunks":chunks,"nextCursor":null},"nextCursor":next_cursor})
}
struct Fixture {
    service: Arc<Service>,
    root: PathBuf,
    task: tokio::task::JoinHandle<()>,
    data: Arc<Mutex<Value>>,
    requests: tokio::sync::mpsc::UnboundedReceiver<Value>,
    // None: success page; null: hold response; object: correlated raw frame.
    frame: Arc<Mutex<Option<Value>>>,
}
impl Fixture {
    async fn new(ready: Value, data: Value) -> Self {
        let root = std::env::temp_dir().join(format!("rpi-child-query-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let service = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("sessions.json"),
            rpi_bin: root.join("must-not-spawn"),
            ..Default::default()
        })
        .unwrap();
        let bare = BareFixture::new(ready, data).await;
        service
            .sessions
            .write()
            .await
            .insert(SESSION.into(), bare.host);
        let (task, data, requests, frame) = (bare.task, bare.data, bare.requests, bare.frame);
        Self {
            service,
            root,
            task,
            data,
            requests,
            frame,
        }
    }
    fn data(&self, data: Value) {
        *self.data.lock().unwrap() = data;
    }
    fn frame(&self, frame: Value) {
        *self.frame.lock().unwrap() = Some(frame);
    }
    async fn close(self) {
        self.service.shutdown().await;
        self.task.abort();
        drop(self.service);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
// Same native page peer as Service tests, but no filesystem/backend/registry.
struct BareFixture {
    host: Arc<HostSession>,
    task: tokio::task::JoinHandle<()>,
    data: Arc<Mutex<Value>>,
    requests: tokio::sync::mpsc::UnboundedReceiver<Value>,
    frame: Arc<Mutex<Option<Value>>>,
}
impl BareFixture {
    async fn new(ready: Value, data: Value) -> Self {
        let (writer, peer) = tokio::io::duplex(8192);
        let (session, replies) = HostSession::test_connection_with_ready(writer, 16, ready);
        let data = Arc::new(Mutex::new(data));
        let response = data.clone();
        let frame = Arc::new(Mutex::new(None::<Value>));
        let response_frame = frame.clone();
        let (tx, requests) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                tx.send(request.clone()).unwrap();
                let data = response.lock().unwrap().clone();
                let frame = response_frame.lock().unwrap().clone();
                if frame == Some(Value::Null) {
                    continue;
                }
                let mut frame =
                    frame.unwrap_or_else(|| json!({"success":true,"data":data,"result":data}));
                frame["type"] = json!("response");
                frame["id"] = request["id"].clone();
                replies.send(frame).unwrap();
            }
        });
        Self {
            host: session,
            task,
            data,
            requests,
            frame,
        }
    }
    async fn close(self) {
        self.host.shutdown().await;
        self.task.abort();
        let _ = self.task.await;
    }
}

#[tokio::test]
async fn exact_profile_forwards_all_three_typed_queries_and_preserves_historical_unknown() {
    let mut f = Fixture::new(ready(), summary_page()).await;
    let page = child_summaries(&f.service, SESSION.into(), sq())
        .await
        .unwrap();
    assert_eq!(page.children[0].state, ChildState::Unknown);
    assert_eq!(
        page.children[0].origin_verification,
        ChildOriginVerification::ForkCopy
    );
    assert_eq!(page.children[0].origin.run_id, "original-run");
    assert!(page.scan_exhausted);
    let mut request = f.requests.recv().await.unwrap();
    assert_eq!(request["type"], SUMMARY_COMMAND);
    assert!(uuid4(request["id"].as_str().unwrap()));
    request.as_object_mut().unwrap().remove("type");
    request.as_object_mut().unwrap().remove("id");
    assert_eq!(request, serde_json::to_value(sq()).unwrap());
    f.data(phase_page());
    let phases = child_phases(&f.service, SESSION.into(), pq())
        .await
        .unwrap();
    assert_eq!(phases.state, ChildState::Unknown); // Even closed/interrupted is not run settlement.
    assert!(phases.scan_exhausted);
    request = f.requests.recv().await.unwrap();
    assert_eq!(request["type"], PHASE_COMMAND);
    request.as_object_mut().unwrap().remove("type");
    request.as_object_mut().unwrap().remove("id");
    assert_eq!(request, serde_json::to_value(pq()).unwrap());
    f.data(body_page(b"{}", 0, 1));
    child_body(&f.service, SESSION.into(), bq()).await.unwrap();
    request = f.requests.recv().await.unwrap();
    assert_eq!(request["type"], BODY_COMMAND);
    request.as_object_mut().unwrap().remove("type");
    request.as_object_mut().unwrap().remove("id");
    assert_eq!(request, serde_json::to_value(bq()).unwrap());
    assert!(f.requests.try_recv().is_err());
    f.close().await;
}
#[tokio::test]
async fn missing_old_wrong_profiles_or_any_missing_command_refuse_without_requests() {
    let mut cases = vec![
        json!({}),
        json!({"supportedCommands":[SUMMARY_COMMAND,PHASE_COMMAND,BODY_COMMAND]}),
    ];
    for (key, value) in [
        ("version", json!(0)),
        ("version", json!(2)),
        ("summaryCommand", json!("other")),
        ("phaseCommand", json!("other")),
        ("bodyCommand", json!("other")),
        ("maxCount", json!(33)),
        ("maxPhaseCandidates", json!(129)),
        ("maxResponseBytes", json!(131073)),
        ("responseBytesScope", json!("frameBytes")),
        ("maxBodyDecodedBytes", json!(65535)),
        ("summaryState", json!("complete")),
        ("recovery", json!("replay")),
        ("events", json!([])),
    ] {
        let mut r = ready();
        r["toolProgramChildren"][key] = value;
        cases.push(r);
    }
    for key in profile().as_object().unwrap().keys() {
        let mut r = ready();
        r["toolProgramChildren"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        cases.push(r);
    }
    for missing in [SUMMARY_COMMAND, PHASE_COMMAND, BODY_COMMAND] {
        let mut r = ready();
        r["supportedCommands"]
            .as_array_mut()
            .unwrap()
            .retain(|s| s != missing);
        cases.push(r);
    }
    for r in cases {
        let mut f = Fixture::new(r, Value::Null).await;
        assert_eq!(
            child_summaries(&f.service, SESSION.into(), sq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::Unsupported
        );
        assert_eq!(
            child_phases(&f.service, SESSION.into(), pq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::Unsupported
        );
        assert_eq!(
            child_body(&f.service, SESSION.into(), bq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::Unsupported
        );
        tokio::task::yield_now().await;
        assert!(f.requests.try_recv().is_err());
        f.close().await;
    }
}
#[tokio::test]
async fn invalid_inputs_fail_before_session_lookup_or_any_request() {
    let mut f = Fixture::new(ready(), Value::Null).await;
    for (limit, bytes) in [(0, 100), (33, 100), (1, 0), (1, 131073)] {
        let mut q = sq();
        q.limit = limit;
        q.max_bytes = bytes;
        assert_eq!(
            child_summaries(&f.service, "nonexistent".into(), q)
                .await
                .unwrap_err()
                .0,
            ReplyKind::InvalidInput
        );
    }
    for key in [
        "sessionId",
        "lane",
        "leafId",
        "afterEntryId",
        "direction",
        "childId",
    ] {
        let mut c = cursor(None, CHILD, 1);
        c[key] = match key {
            "direction" => json!("newestFirst"),
            "childId" => json!(OTHER),
            "lane" => json!("bad\n"),
            _ => json!("foreign"),
        };
        let mut q = sq();
        q.cursor = Some(serde_json::from_value(c).unwrap());
        assert_eq!(
            child_summaries(&f.service, SESSION.into(), q)
                .await
                .unwrap_err()
                .0,
            ReplyKind::InvalidInput
        );
    }
    let mut q = pq();
    q.child_id = "tpchild-not-uuid".into();
    assert_eq!(
        child_phases(&f.service, SESSION.into(), q)
            .await
            .unwrap_err()
            .0,
        ReplyKind::InvalidInput
    );
    let mut q = bq();
    q.max_bytes = 65537;
    assert_eq!(
        child_body(&f.service, SESSION.into(), q)
            .await
            .unwrap_err()
            .0,
        ReplyKind::InvalidInput
    );
    let mut q = bq();
    q.limit = 9;
    assert_eq!(
        child_body(&f.service, SESSION.into(), q)
            .await
            .unwrap_err()
            .0,
        ReplyKind::InvalidInput
    );
    assert!(f.requests.try_recv().is_err());
    f.close().await;
}
#[tokio::test]
async fn malformed_oversized_foreign_summary_pages_fail_closed() {
    let mut cases = Vec::new();
    for (key, value) in [
        ("version", json!(2)),
        ("childId", json!("tpchild-fake")),
        ("state", json!("completed")),
        ("originVerification", json!("assumed")),
        ("toolName", json!("x".repeat(513))),
        ("rootCallId", json!("bad\n")),
        ("parentChildId", json!(CHILD)),
        ("unexpected", json!(true)),
    ] {
        let mut p = summary_page();
        p["children"][0][key] = value;
        cases.push(p);
    }
    let mut p = summary_page();
    p["leafId"] = json!("foreign");
    cases.push(p);
    let mut p = summary_page();
    p["children"] = json!(vec![p["children"][0].clone(); 33]);
    cases.push(p);
    let mut p = summary_page();
    p["unexpected"] = json!("x".repeat(MAX_RESPONSE_BYTES));
    cases.push(p);
    let mut p = summary_page();
    p["scanExhausted"] = json!(false);
    cases.push(p);
    let mut p = summary_page();
    p["nextCursor"] = cursor(None, CHILD, 10);
    cases.push(p);
    for p in cases {
        let f = Fixture::new(ready(), p).await;
        assert_eq!(
            child_summaries(&f.service, SESSION.into(), sq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::HostUnavailable
        );
        f.close().await;
    }
}
#[tokio::test]
async fn sparse_scan_continuation_is_valid_but_foreign_or_stalled_cursors_are_not() {
    let p = json!({"leafId":"leaf","childId":CHILD,"phases":[],"nextCursor":cursor(Some(CHILD),PHASE2,200),"scanned":128,"scanExhausted":false,"state":"unknown"});
    let f = Fixture::new(ready(), p.clone()).await;
    let page = child_phases(&f.service, SESSION.into(), pq())
        .await
        .unwrap();
    assert!(page.phases.is_empty());
    assert!(!page.scan_exhausted);
    assert_eq!(page.state, ChildState::Unknown);
    let mut q = pq();
    q.cursor = page.next_cursor;
    f.data(json!({"leafId":"leaf","childId":CHILD,"phases":[],"nextCursor":null,"scanned":0,"scanExhausted":true,"state":"unknown"}));
    assert_eq!(
        child_phases(&f.service, SESSION.into(), q.clone())
            .await
            .unwrap()
            .state,
        ChildState::Unknown
    );
    for (key, value) in [
        ("sessionId", json!("foreign")),
        ("lane", json!("foreign")),
        ("direction", json!("newestFirst")),
        ("childId", json!(OTHER)),
        ("leafId", json!("foreign")),
    ] {
        let mut p = p.clone();
        p["nextCursor"][key] = value;
        f.data(p);
        assert_eq!(
            child_phases(&f.service, SESSION.into(), q.clone())
                .await
                .unwrap_err()
                .0,
            ReplyKind::HostUnavailable
        );
    }
    f.data(p); // same sequence as input: cannot loop forever.
    assert!(child_phases(&f.service, SESSION.into(), q).await.is_err());
    f.close().await;
}
#[tokio::test]
async fn body_chunks_validate_hash_offsets_bounds_and_all_source_bindings() {
    let bytes = vec![b'x'; 65536];
    let mut f = Fixture::new(ready(), body_page(&bytes, 0, 2)).await;
    // Native duplicates data/result: the frame is >128 KiB but the PAGE is not.
    let page = f.data.lock().unwrap().clone();
    assert!(serde_json::to_vec(&page).unwrap().len() < MAX_RESPONSE_BYTES);
    assert!(
        serde_json::to_vec(&json!({"data":page,"result":page}))
            .unwrap()
            .len()
            > MAX_RESPONSE_BYTES
    );
    child_body(&f.service, SESSION.into(), bq()).await.unwrap();
    let whole = body_page(&bytes, 0, 2);
    let mut cases = Vec::new();
    for (key, value) in [("childId", json!(OTHER)), ("phaseEntryId", json!(PHASE2))] {
        let mut p = whole.clone();
        p[key] = value;
        cases.push(p);
    }
    for (key, value) in [
        ("leafId", json!("foreign")),
        ("byteOffset", json!(1)),
        ("byteLength", json!(65535)),
        ("totalBytes", json!(65537)),
        (
            "nextCursor",
            json!({"sessionId":SESSION,"lane":"main","leafId":"leaf","descriptorId":"foreign","nextIndex":2}),
        ),
    ] {
        let mut p = whole.clone();
        p["body"][key] = value;
        cases.push(p);
    }
    for (key, value) in [
        ("version", json!(2)),
        ("bodyId", json!("not-uuid")),
        ("childId", json!(OTHER)),
        ("descriptorId", json!("foreign")),
        ("kind", json!("closed")),
        ("codec", json!("json")),
        ("encoding", json!("utf8")),
        ("byteLength", json!(0)),
        ("chunkBytes", json!(1)),
        ("chunks", json!(1025)),
        ("sha256", json!("A".repeat(64))),
    ] {
        let mut p = whole.clone();
        p["body"]["descriptor"][key] = value;
        cases.push(p);
    }
    for (key, value) in [
        ("index", json!(1)),
        ("byteOffset", json!(1)),
        ("byteLength", json!(1)),
        ("data", json!("!".repeat(43692))),
        ("sha256", json!("0".repeat(64))),
    ] {
        let mut p = whole.clone();
        p["body"]["chunks"][0][key] = value;
        cases.push(p);
    }
    let mut p = whole.clone();
    p["body"]["chunks"][1] = p["body"]["chunks"][0].clone();
    cases.push(p);
    let mut p = whole.clone();
    p["nextCursor"] = json!({"direction":"oldestFirst","childId":CHILD,"phaseEntryId":PHASE,
        "body":{"sessionId":SESSION,"lane":"main","leafId":"leaf","descriptorId":bq().descriptor_id,"nextIndex":2}});
    cases.push(p);
    let mut p = whole.clone();
    p["body"]["chunks"][0]["data"] = json!("x".repeat(MAX_RESPONSE_BYTES));
    cases.push(p);
    for p in cases {
        f.data(p);
        assert_eq!(
            child_body(&f.service, SESSION.into(), bq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::HostUnavailable
        );
    }
    f.data(whole.clone());
    let mut q = bq();
    q.max_bytes = 32768;
    assert!(child_body(&f.service, SESSION.into(), q).await.is_err());
    let partial = body_page(&bytes, 0, 1);
    f.data(partial);
    let first = child_body(&f.service, SESSION.into(), bq()).await.unwrap();
    assert_eq!(first.next_cursor.as_ref().unwrap().body.next_index, 1);
    let mut q = bq();
    q.cursor = first.next_cursor;
    f.data(body_page(&bytes, 1, 1));
    child_body(&f.service, SESSION.into(), q.clone())
        .await
        .unwrap();
    let continued = body_page(&vec![b'x'; 98304], 1, 1);
    for (key, value) in [
        ("sessionId", json!("foreign")),
        ("lane", json!("other")),
        ("descriptorId", json!("foreign")),
        ("nextIndex", json!(1)),
    ] {
        let mut p = continued.clone();
        p["nextCursor"]["body"][key] = value;
        f.data(p);
        assert!(child_body(&f.service, SESSION.into(), q.clone())
            .await
            .is_err());
    }
    while f.requests.try_recv().is_ok() {}
    q.cursor.as_mut().unwrap().body.session_id = "foreign".into();
    assert_eq!(
        child_body(&f.service, SESSION.into(), q)
            .await
            .unwrap_err()
            .0,
        ReplyKind::InvalidInput
    );
    assert!(f.requests.try_recv().is_err());
    f.close().await;
}
#[tokio::test]
async fn summary_and_phase_order_uniqueness_schema_and_scan_shape_are_checked() {
    let f = Fixture::new(ready(), Value::Null).await;
    let mut p = summary_page();
    let mut second = p["children"][0].clone();
    second["childId"] = json!(OTHER);
    second["seq"] = json!(9);
    p["children"].as_array_mut().unwrap().push(second);
    f.data(p.clone());
    assert!(child_summaries(&f.service, SESSION.into(), sq())
        .await
        .is_err());
    let mut q = sq();
    q.direction = Direction::NewestFirst;
    child_summaries(&f.service, SESSION.into(), q)
        .await
        .unwrap();
    p["children"][1]["seq"] = json!(10);
    f.data(p);
    assert!(child_summaries(&f.service, SESSION.into(), sq())
        .await
        .is_err());
    let mut phase_cases = Vec::new();
    for (key, value) in [
        ("childId", json!(OTHER)),
        ("scanned", json!(129)),
        ("scanned", json!(0)),
        ("scanExhausted", json!(false)),
        ("state", json!("completed")),
    ] {
        let mut p = phase_page();
        p[key] = value;
        phase_cases.push(p);
    }
    for (key, value) in [
        ("entryId", json!("tpphase-fake")),
        ("childId", json!(OTHER)),
        ("version", json!(2)),
        ("index", json!(0)),
        ("failed", Value::Null),
        ("interrupted", Value::Null),
        ("isError", json!(false)),
        ("cancelled", json!(false)),
    ] {
        let mut p = phase_page();
        p["phases"][0][key] = value;
        phase_cases.push(p);
    }
    let mut p = phase_page();
    p["scanned"] = json!(2);
    let mut second = p["phases"][0].clone();
    second["entryId"] = json!(PHASE2);
    second["seq"] = json!(21);
    p["phases"].as_array_mut().unwrap().push(second);
    phase_cases.push(p); // repeated phase index
    for p in phase_cases {
        f.data(p);
        assert!(child_phases(&f.service, SESSION.into(), pq())
            .await
            .is_err());
    }
    let bp = body_page(b"{}", 0, 1);
    let mut p = phase_page();
    p["phases"][0] = json!({"entryId":PHASE,"seq":20,"version":1,"childId":CHILD,"index":1,"phase":"final",
        "descriptor":bp["body"]["descriptor"],"isError":false,"cancelled":false,"failed":null,"interrupted":null});
    f.data(p.clone());
    child_phases(&f.service, SESSION.into(), pq())
        .await
        .unwrap();
    p["phases"][0]["descriptor"]["kind"] = json!("update");
    f.data(p);
    assert!(child_phases(&f.service, SESSION.into(), pq())
        .await
        .is_err());
    f.close().await;
}
#[test]
fn dto_schemas_are_strict_at_every_nested_boundary() {
    let mut v = serde_json::to_value(sq()).unwrap();
    v["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ChildPageQuery>(v).is_err());
    let mut p = summary_page();
    p["children"][0]["origin"]["unexpected"] = json!(1);
    assert!(serde_json::from_value::<ChildPage>(p).is_err());
    let mut q = serde_json::to_value(bq()).unwrap();
    q["unexpected"] = json!(1);
    assert!(serde_json::from_value::<ChildBodyQuery>(q).is_err());
}

#[tokio::test]
async fn native_rejections_and_malformed_failure_frames_are_sanitized_host_errors() {
    let mut f = Fixture::new(ready(), summary_page()).await;
    for error in [
        "storage I/O failed at D:/private/session.jsonl token=secret",
        "invalid native query",
        "unclassified native rejection",
    ] {
        f.frame(json!({"success":false,"error":error}));
        let err = child_summaries(&f.service, SESSION.into(), sq())
            .await
            .unwrap_err();
        assert_eq!(
            err,
            failure(ReplyKind::HostUnavailable, "native child query rejected")
        );
        assert_eq!(f.requests.recv().await.unwrap()["type"], SUMMARY_COMMAND);
        assert!(f.requests.try_recv().is_err()); // No request retry.
    }
    for frame in [
        json!({"success":false}),
        json!({"success":false,"error":42}),
        json!({"success":false,"error":""}),
        json!({"success":false,"error":"   "}),
        json!({"success":false,"error":"x".repeat(4097)}),
        json!({"error":"secret"}),
    ] {
        f.frame(frame);
        assert_eq!(
            child_summaries(&f.service, SESSION.into(), sq())
                .await
                .unwrap_err(),
            malformed()
        );
        f.requests.recv().await.unwrap();
        assert!(f.requests.try_recv().is_err());
    }
    f.close().await;
}

#[tokio::test(start_paused = true)]
async fn query_timeout_caller_cancellation_and_transport_closure_do_not_retry() {
    let mut f = Fixture::new(ready(), summary_page()).await;
    f.frame(Value::Null);
    let service = f.service.clone();
    let timed = tokio::spawn(async move { child_summaries(&service, SESSION.into(), sq()).await });
    f.requests.recv().await.unwrap();
    tokio::time::advance(Duration::from_secs(31)).await;
    assert_eq!(
        timed.await.unwrap().unwrap_err(),
        failure(ReplyKind::HostUnavailable, "native child query failed")
    );
    assert!(f.requests.try_recv().is_err());

    let service = f.service.clone();
    let cancelled =
        tokio::spawn(async move { child_summaries(&service, SESSION.into(), sq()).await });
    f.requests.recv().await.unwrap();
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    *f.frame.lock().unwrap() = None;
    // A cancelled caller must not consume or settle the next correlation.
    child_summaries(&f.service, SESSION.into(), sq())
        .await
        .unwrap();
    f.requests.recv().await.unwrap();
    assert!(f.requests.try_recv().is_err());

    // Closing the fixture pipe makes the existing send_timeout path fail closed.
    f.task.abort();
    while !f.task.is_finished() {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        child_summaries(&f.service, SESSION.into(), sq())
            .await
            .unwrap_err(),
        failure(ReplyKind::HostUnavailable, "native child query failed")
    );
    f.close().await;
}

#[tokio::test]
async fn nonempty_sparse_phase_byte_cutoffs_continue_past_other_child_in_both_directions() {
    const OTHER_PHASE: &str = "tpphase-66666666-6666-4666-8666-666666666666";
    for direction in [Direction::OldestFirst, Direction::NewestFirst] {
        let mut q = pq();
        q.direction = direction;
        let (first_seq, skipped_seq, last_seq, last_index) = match direction {
            Direction::OldestFirst => (20, 30, 40, 5),
            Direction::NewestFirst => (40, 30, 20, 3),
        };
        let mut p = phase_page();
        let mut descriptor = body_page(b"{}", 0, 1)["body"]["descriptor"].clone();
        descriptor["kind"] = json!("update");
        p["phases"][0] = json!({"entryId":PHASE,"seq":first_seq,"version":1,
            "childId":CHILD,"index":4,"phase":"update","descriptor":descriptor,
            "isError":null,"cancelled":null,"failed":null,"interrupted":null});
        let mut other = phase_page()["phases"][0].clone();
        other["entryId"] = json!(OTHER_PHASE);
        other["seq"] = json!(skipped_seq);
        other["childId"] = json!(OTHER);
        let scanned = [p["phases"][0].clone(), other];
        let last_scanned = scanned.last().unwrap();
        assert_ne!(last_scanned["childId"], json!(CHILD));
        let mut c = cursor(
            Some(CHILD),
            last_scanned["entryId"].as_str().unwrap(),
            last_scanned["seq"].as_u64().unwrap(),
        );
        c["direction"] = serde_json::to_value(direction).unwrap();
        // Native binds childId to the selection, not the scanned entry's child.
        p["nextCursor"] = c;
        p["scanned"] = json!(scanned.len());
        p["scanExhausted"] = json!(false);
        let mut last = phase_page();
        last["phases"][0] = p["phases"][0].clone();
        last["phases"][0]["entryId"] = json!(PHASE2);
        last["phases"][0]["seq"] = json!(last_seq);
        last["phases"][0]["index"] = json!(last_index);
        q.max_bytes = serde_json::to_vec(&p).unwrap().len();
        let mut would_append = p.clone();
        would_append["phases"]
            .as_array_mut()
            .unwrap()
            .push(last["phases"][0].clone());
        assert!(serde_json::to_vec(&would_append).unwrap().len() > q.max_bytes);
        let mut f = Fixture::new(ready(), p).await;
        let first = child_phases(&f.service, SESSION.into(), q.clone())
            .await
            .unwrap();
        assert_eq!(first.phases.len(), 1);
        assert!(!first.scan_exhausted);
        q.cursor = first.next_cursor;
        assert_eq!(q.cursor.as_ref().unwrap().after_entry_id, OTHER_PHASE);
        f.requests.recv().await.unwrap();
        f.data(last);
        let continued = child_phases(&f.service, SESSION.into(), q.clone())
            .await
            .unwrap();
        assert!(continued.scan_exhausted);
        assert_eq!(continued.phases[0].seq, last_seq);
        assert_eq!(
            f.requests.recv().await.unwrap()["cursor"],
            serde_json::to_value(q.cursor).unwrap()
        );
        f.close().await;
    }
}

#[tokio::test]
async fn summary_exact_byte_budget_full_count_and_cursor_roundtrip_to_eof() {
    let mut p = summary_page();
    p["scanExhausted"] = json!(false);
    p["nextCursor"] = cursor(None, CHILD, 10);
    let n = serde_json::to_vec(&p).unwrap().len();
    let mut f = Fixture::new(ready(), p).await;
    let mut q = sq();
    q.limit = 1;
    q.max_bytes = n;
    let page = child_summaries(&f.service, SESSION.into(), q.clone())
        .await
        .unwrap();
    assert_eq!(page.children.len(), q.limit);
    assert!(!page.scan_exhausted); // Exactly limit at actual EOF still needs a cursor.
    f.requests.recv().await.unwrap();
    q.max_bytes = n - 1;
    assert_eq!(
        child_summaries(&f.service, SESSION.into(), q.clone())
            .await
            .unwrap_err()
            .0,
        ReplyKind::HostUnavailable
    );
    f.requests.recv().await.unwrap();
    q.max_bytes = n;
    q.cursor = page.next_cursor;
    f.data(json!({"leafId":"leaf","children":[],"nextCursor":null,"scanExhausted":true}));
    let eof = child_summaries(&f.service, SESSION.into(), q.clone())
        .await
        .unwrap();
    assert!(eof.children.is_empty() && eof.scan_exhausted && eof.next_cursor.is_none());
    assert_eq!(
        f.requests.recv().await.unwrap()["cursor"],
        serde_json::to_value(q.cursor).unwrap()
    );
    f.close().await;
}

#[tokio::test]
async fn unrelated_top_level_child_events_do_not_change_exact_query_profile() {
    let mut r = ready();
    r["toolProgramChildEvents"] = json!({"version":1,"events":["tool_program_child_phase"]});
    assert_eq!(r["toolProgramChildren"], profile());
    let mut f = Fixture::new(r, summary_page()).await;
    child_summaries(&f.service, SESSION.into(), sq())
        .await
        .unwrap();
    assert_eq!(f.requests.recv().await.unwrap()["type"], SUMMARY_COMMAND);
    f.close().await;
}

#[tokio::test]
async fn whole_body_stream_retains_full_descriptor_hash_and_partial_final_chunk() {
    let bytes: Vec<_> = (0..CHUNK_BYTES * 2 + 137)
        .map(|i| (i % 251) as u8)
        .collect();
    let f = Fixture::new(ready(), body_page(&bytes, 0, 1)).await;
    let mut q = bq();
    q.limit = 1;
    q.max_bytes = CHUNK_BYTES;
    let mut assembled = Vec::new();
    let mut aggregate = Sha256::new();
    for index in 0..3 {
        f.data(body_page(&bytes, index, 1));
        let p = child_body(&f.service, SESSION.into(), q.clone())
            .await
            .unwrap();
        assert_eq!(
            p.body.descriptor.sha256,
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(p.body.descriptor.byte_length, bytes.len());
        let chunk = STANDARD.decode(&p.body.chunks[0].data).unwrap();
        assert_ne!(
            p.body.descriptor.sha256,
            format!("{:x}", Sha256::digest(&chunk))
        );
        assert_eq!(chunk.len(), if index == 2 { 137 } else { CHUNK_BYTES });
        aggregate.update(&chunk);
        assembled.extend_from_slice(&chunk);
        assert_eq!(p.next_cursor.is_some(), index < 2);
        if index < 2 {
            // A partial page makes no whole-body availability/integrity claim.
            assert_ne!(
                format!("{:x}", aggregate.clone().finalize()),
                p.body.descriptor.sha256
            );
        } else {
            assert_eq!(
                format!("{:x}", aggregate.clone().finalize()),
                p.body.descriptor.sha256
            );
        }
        q.cursor = p.next_cursor;
    }
    assert_eq!(assembled, bytes);
    f.close().await;
}

#[tokio::test]
async fn service_route_identity_must_match_owner_binding_before_any_child_query() {
    use crate::service::{tool_program_events as events, tool_program_live, tool_program_snapshot};
    for source in [
        json!("foreign-session"),
        Value::Null,
        json!("invalid\nsource"),
    ] {
        let mut r = ready();
        r["sessionId"] = source;
        r["toolProgramChildEvents"] = events::profile();
        r["supportedEvents"] = json!([
            "tool_program_child_begin",
            "tool_program_child_phase",
            "tool_program_child_resync_required"
        ]);
        let mut f = Fixture::new(r, summary_page()).await;
        assert_eq!(
            child_summaries(&f.service, SESSION.into(), sq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::HostUnavailable
        );
        assert_eq!(
            child_phases(&f.service, SESSION.into(), pq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::HostUnavailable
        );
        assert_eq!(
            child_body(&f.service, SESSION.into(), bq())
                .await
                .unwrap_err()
                .0,
            ReplyKind::HostUnavailable
        );
        let snapshot_query = tool_program_snapshot::ChildSnapshotQuery {
            children: ChildPageQuery {
                leaf_id: None,
                cursor: None,
                direction: Direction::NewestFirst,
                limit: 8,
                max_bytes: 32768,
            },
            phase_limit: 8,
        };
        assert_eq!(
            tool_program_snapshot::child_snapshot(
                &f.service,
                SESSION.into(),
                snapshot_query.clone()
            )
            .await
            .unwrap_err()
            .0,
            ReplyKind::HostUnavailable
        );
        assert!(matches!(
            tool_program_live::child_subscription(&f.service, SESSION.into(), snapshot_query).await,
            Err((ReplyKind::HostUnavailable, _))
        ));
        assert!(f.requests.try_recv().is_err());
        f.close().await;
    }
}

#[derive(Clone, Copy)]
enum HostReader {
    Summaries,
    Phases,
    Body,
}
impl HostReader {
    fn page(self) -> Value {
        match self {
            Self::Summaries => summary_page(),
            Self::Phases => phase_page(),
            Self::Body => body_page(b"{}", 0, 1),
        }
    }
    async fn read(self, host: &HostSession, invalid: bool) -> ServiceResult<Value> {
        match self {
            Self::Summaries => {
                let mut q = sq();
                if invalid {
                    q.max_bytes = MAX_RESPONSE_BYTES + 1;
                }
                child_summaries_for_host(host, q)
                    .await
                    .map(|p| serde_json::to_value(p).unwrap())
            }
            Self::Phases => {
                let mut q = pq();
                if invalid {
                    q.limit = 33;
                }
                child_phases_for_host(host, q)
                    .await
                    .map(|p| serde_json::to_value(p).unwrap())
            }
            Self::Body => {
                let mut q = bq();
                if invalid {
                    q.max_bytes = 65537;
                }
                child_body_for_host(host, q)
                    .await
                    .map(|p| serde_json::to_value(p).unwrap())
            }
        }
    }
}
const HOST_READERS: [HostReader; 3] = [HostReader::Summaries, HostReader::Phases, HostReader::Body];
fn host_ready() -> Value {
    let mut r = ready();
    r["sessionId"] = json!(SESSION);
    r
}

#[tokio::test]
async fn bare_host_all_readers_preserve_valid_native_fixture_pages() {
    for reader in HOST_READERS {
        let mut f = BareFixture::new(host_ready(), reader.page()).await;
        assert_eq!(reader.read(&f.host, false).await.unwrap(), reader.page());
        let sent = f.requests.recv().await.unwrap();
        assert!([SUMMARY_COMMAND, PHASE_COMMAND, BODY_COMMAND]
            .contains(&sent["type"].as_str().unwrap()));
        assert!(f.requests.try_recv().is_err());
        f.close().await;
    }
}

#[tokio::test]
async fn bare_host_all_readers_profile_and_budget_preflight_send_zero_requests() {
    for reader in HOST_READERS {
        for supported in [true, false] {
            let mut profile = host_ready();
            if !supported {
                profile["toolProgramChildren"]["maxCount"] = json!(31);
            }
            let mut f = BareFixture::new(profile, reader.page()).await;
            assert_eq!(
                reader.read(&f.host, true).await.unwrap_err().0,
                ReplyKind::InvalidInput
            );
            if !supported {
                assert_eq!(
                    reader.read(&f.host, false).await.unwrap_err().0,
                    ReplyKind::Unsupported
                );
            }
            assert!(f.requests.try_recv().is_err());
            f.close().await;
        }
    }
}

#[tokio::test]
async fn bare_host_source_identity_and_foreign_cursors_fail_without_rpc() {
    for id in [
        Value::Null,
        json!(""),
        json!("bad\nidentity"),
        json!("x".repeat(513)),
    ] {
        let mut profile = host_ready();
        profile["sessionId"] = id;
        let mut f = BareFixture::new(profile, summary_page()).await;
        for reader in HOST_READERS {
            assert_eq!(
                reader.read(&f.host, false).await.unwrap_err().0,
                ReplyKind::HostUnavailable
            );
        }
        assert!(f.requests.try_recv().is_err());
        f.close().await;
    }
    let mut f = BareFixture::new(host_ready(), summary_page()).await;
    let mut q = sq();
    let mut c = cursor(None, CHILD, 10);
    c["sessionId"] = json!("foreign");
    q.cursor = Some(serde_json::from_value(c).unwrap());
    assert_eq!(
        child_summaries_for_host(&f.host, q).await.unwrap_err().0,
        ReplyKind::InvalidInput
    );
    let mut q = pq();
    let mut c = cursor(Some(CHILD), PHASE, 20);
    c["sessionId"] = json!("foreign");
    q.cursor = Some(serde_json::from_value(c).unwrap());
    assert_eq!(
        child_phases_for_host(&f.host, q).await.unwrap_err().0,
        ReplyKind::InvalidInput
    );
    let mut q = bq();
    q.cursor = Some(ChildBodyCursor {
        direction: Direction::OldestFirst,
        child_id: CHILD.into(),
        phase_entry_id: PHASE.into(),
        body: BodyCursor {
            session_id: "foreign".into(),
            lane: "main".into(),
            leaf_id: "leaf".into(),
            descriptor_id: q.descriptor_id.clone(),
            next_index: 0,
        },
    });
    assert_eq!(
        child_body_for_host(&f.host, q).await.unwrap_err().0,
        ReplyKind::InvalidInput
    );
    assert!(f.requests.try_recv().is_err());
    f.close().await;
}

#[tokio::test(start_paused = true)]
async fn bare_host_all_readers_timeout_cancel_sanitize_and_reuse_correlation() {
    for reader in HOST_READERS {
        let mut f = BareFixture::new(host_ready(), reader.page()).await;
        *f.frame.lock().unwrap() = Some(Value::Null);
        let host = f.host.clone();
        let timed = tokio::spawn(async move { reader.read(&host, false).await });
        f.requests.recv().await.unwrap();
        tokio::time::advance(Duration::from_secs(31)).await;
        assert_eq!(
            timed.await.unwrap().unwrap_err(),
            failure(ReplyKind::HostUnavailable, "native child query failed")
        );
        let host = f.host.clone();
        let cancelled = tokio::spawn(async move { reader.read(&host, false).await });
        f.requests.recv().await.unwrap();
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        *f.frame.lock().unwrap() = Some(json!({"success":false,"error":"SECRET backend/path"}));
        let err = reader.read(&f.host, false).await.unwrap_err();
        assert_eq!(
            err,
            failure(ReplyKind::HostUnavailable, "native child query rejected")
        );
        f.requests.recv().await.unwrap();
        *f.frame.lock().unwrap() = None;
        reader.read(&f.host, false).await.unwrap();
        f.requests.recv().await.unwrap();
        assert!(f.requests.try_recv().is_err());
        f.close().await;
    }
}

#[tokio::test]
async fn bare_host_closed_and_retired_readers_fail_without_requests() {
    for mode in ["closed", "retired"] {
        let mut f = BareFixture::new(host_ready(), summary_page()).await;
        match mode {
            "closed" => f.host.shutdown().await,
            "retired" => f.host.block_resource_storage(),
            _ => unreachable!(),
        }
        for reader in HOST_READERS {
            assert_eq!(
                reader.read(&f.host, false).await.unwrap_err().0,
                ReplyKind::HostUnavailable
            );
        }
        assert!(f.requests.try_recv().is_err());
        f.close().await;
    }
}
