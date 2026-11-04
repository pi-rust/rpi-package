use super::*;
use crate::{host::TestEvents, service::BackendConfig};
use std::{path::PathBuf, sync::Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};

const SESSION: &str = "session";
const CHILD: &str = "tpchild-11111111-1111-4111-8111-111111111111";
const REQUESTED: &str = "tpphase-22222222-2222-4222-8222-222222222222";
const PREPARED: &str = "tpphase-33333333-3333-4333-8333-333333333333";
const FINAL: &str = "tpphase-44444444-4444-4444-8444-444444444444";
const CLOSED: &str = "tpphase-55555555-5555-4555-8555-555555555555";

fn ready() -> Value {
    json!({"toolProgramChildren":{"version":1,"summaryCommand":"get_tool_program_children",
        "phaseCommand":"get_tool_program_child_phases","bodyCommand":"get_tool_program_child_body",
        "maxCount":32,"maxPhaseCandidates":128,"maxResponseBytes":131072,"responseBytesScope":"pageBytes",
        "maxBodyDecodedBytes":65536,"summaryState":"unknown","recovery":"readOnly"},
        "toolProgramChildEvents":events::profile(),
        "supportedEvents":["tool_program_child_begin","tool_program_child_phase","tool_program_child_resync_required"],
        "supportedCommands":["get_tool_program_children","get_tool_program_child_phases","get_tool_program_child_body"]})
}
fn request() -> ChildSnapshotQuery {
    ChildSnapshotQuery {
        children: ChildPageQuery {
            leaf_id: None,
            cursor: None,
            direction: Direction::NewestFirst,
            limit: 8,
            max_bytes: 32768,
        },
        phase_limit: 2,
    }
}
fn summary() -> Value {
    json!({"version":1,"childId":CHILD,"seq":10,"origin":{"runId":"run","assistantEntryId":"assistant","toolIndex":0},
        "originVerification":"nativeCheckpoint","rootCallId":"root-call","rootToolName":"program",
        "toolName":"read","ordinal":0,"parentChildId":null,"state":"unknown"})
}
fn descriptor(kind: &str) -> Value {
    json!({"version":1,"bodyId":"66666666-6666-4666-8666-666666666666",
        "descriptorId":"tpbody-66666666-6666-4666-8666-666666666666-commit",
        "childId":CHILD,"kind":kind,"codec":"native-json-v1","encoding":"base64",
        "byteLength":2,"chunkBytes":32768,"chunks":1,"sha256":"a".repeat(64)})
}
fn phase(kind: &str) -> ChildPhase {
    let (id, seq, index) = match kind {
        "requested" => (REQUESTED, 11, 0),
        "prepared" => (PREPARED, 20, 1),
        "final" => (FINAL, 30, 2),
        "closed" => (CLOSED, 40, 3),
        _ => panic!(),
    };
    serde_json::from_value(
        json!({"entryId":id,"seq":seq,"version":1,"childId":CHILD,"index":index,"phase":kind,
        "descriptor":if kind=="closed" {Value::Null} else {descriptor(kind)},
        "isError":if kind=="final" {json!(false)} else {Value::Null},
        "cancelled":if kind=="final" {json!(false)} else {Value::Null},
        "failed":if kind=="closed" {json!(false)} else {Value::Null},
        "interrupted":if kind=="closed" {json!(false)} else {Value::Null}}),
    )
    .unwrap()
}
fn event(phase: &ChildPhase) -> Value {
    let mut data = serde_json::to_value(phase).unwrap();
    data.as_object_mut().unwrap().remove("entryId");
    data.as_object_mut().unwrap().remove("seq");
    json!({"type":"tool_program_child_phase","schemaVersion":1,"sourceSessionId":SESSION,"lane":"main",
        "entryId":phase.entry_id,"parentEntryId":"parent","seq":phase.seq,
        "customType":"rpi.tool_program.child_phase.v1","data":data})
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    Empty,
    Overlap,
    Sparse,
    Evict,
    Overflow,
    Resync,
    Foreign,
    BigGeneric,
    Hold,
}
struct Fixture {
    service: Arc<Service>,
    root: PathBuf,
    task: tokio::task::JoinHandle<()>,
    replies: TestEvents,
    mode: Arc<Mutex<Mode>>,
    requests: tokio::sync::mpsc::UnboundedReceiver<Value>,
}
impl Fixture {
    async fn new(mode: Mode, ready: Value) -> Self {
        let root =
            std::env::temp_dir().join(format!("rpi-child-snapshot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let service = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("registry.json"),
            rpi_bin: root.join("must-not-launch"),
            ..Default::default()
        })
        .unwrap();
        let (writer, peer) = tokio::io::duplex(8192);
        let (host, replies) = HostSession::test_connection_with_ready(writer, 1024, ready);
        service.sessions.write().await.insert(SESSION.into(), host);
        let mode = Arc::new(Mutex::new(mode));
        let selected = mode.clone();
        let publish = replies.clone();
        let (tx, requests) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let q: Value = serde_json::from_str(&line).unwrap();
                tx.send(q.clone()).unwrap();
                let mode = *selected.lock().unwrap();
                let data = match q["type"].as_str().unwrap() {
                    "get_tool_program_children" => {
                        if mode == Mode::Empty {
                            json!({"leafId":null,"children":[],"nextCursor":null,"scanExhausted":true})
                        } else if q["limit"] == 1 {
                            json!({"leafId":"leaf","children":[summary()],"scanExhausted":false,
                                "nextCursor":{"sessionId":SESSION,"lane":"main","leafId":"leaf","direction":"newestFirst",
                                    "childId":null,"afterEntryId":CHILD,"afterSeq":10}})
                        } else {
                            json!({"leafId":"leaf","children":[summary()],"nextCursor":null,"scanExhausted":true})
                        }
                    }
                    "get_tool_program_child_phases" => {
                        match mode {
                            Mode::Hold => continue,
                            Mode::Overlap => {
                                for kind in ["prepared", "final", "closed"] {
                                    publish.send_projected(event(&phase(kind))).await;
                                }
                            }
                            Mode::Evict => {
                                for _ in 0..513 {
                                    publish
                                        .send_projected(json!({"type":"session_entry_committed"}))
                                        .await;
                                }
                            }
                            Mode::BigGeneric => {
                                publish.send_projected(json!({"type":"tool_execution_update","body":"x".repeat(900000)})).await;
                            }
                            Mode::Foreign => {
                                let mut frame = event(&phase("final"));
                                frame["sourceSessionId"] = json!("private-wrong-source");
                                publish.send_projected(frame).await;
                            }
                            Mode::Resync => {
                                publish.send_projected(json!({"type":"tool_program_child_resync_required","reason":"commit_stream_lagged","skipped":9})).await;
                            }
                            Mode::Overflow => {
                                for n in 0..80 {
                                    let mut data = summary();
                                    data.as_object_mut().unwrap().remove("seq");
                                    data.as_object_mut().unwrap().remove("originVerification");
                                    data.as_object_mut().unwrap().remove("state");
                                    let id = format!("tpchild-{}", uuid::Uuid::new_v4());
                                    data["childId"] = json!(id);
                                    for key in ["rootCallId", "rootToolName", "toolName"] {
                                        data[key] = json!("x".repeat(512));
                                    }
                                    data["origin"]["runId"] = json!("x".repeat(512));
                                    data["origin"]["assistantEntryId"] = json!("x".repeat(512));
                                    publish.send_projected(json!({"type":"tool_program_child_begin","schemaVersion":1,"sourceSessionId":SESSION,
                                        "lane":"main","entryId":id,"parentEntryId":"parent","seq":1000+n,
                                        "customType":"rpi.tool_program.child_begin.v1","data":data})).await;
                                }
                            }
                            _ => {}
                        }
                        if mode == Mode::Sparse {
                            json!({"leafId":"leaf","childId":CHILD,"phases":[],"scanned":128,"scanExhausted":false,"state":"unknown",
                                "nextCursor":{"sessionId":SESSION,"lane":"main","leafId":"leaf","direction":"newestFirst","childId":CHILD,
                                    "afterEntryId":PREPARED,"afterSeq":15}})
                        } else {
                            json!({"leafId":"leaf","childId":CHILD,"phases":[phase("prepared"),phase("requested")],
                            "nextCursor":null,"scanned":2,"scanExhausted":true,"state":"unknown"})
                        }
                    }
                    other => panic!("unexpected fallback/execution: {other}"),
                };
                publish
                    .send(json!({"type":"response","id":q["id"],"success":true,"data":data}))
                    .unwrap();
            }
        });
        Self {
            service,
            root,
            task,
            replies,
            mode,
            requests,
        }
    }
    async fn close(self) {
        self.task.abort();
        let _ = self.task.await;
        self.service.shutdown().await;
        drop(self.replies);
        drop(self.service);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[test]
fn outcome_requires_both_adjacent_final_and_close_not_exhaustion() {
    let final_phase = phase("final");
    let closed = phase("closed");
    assert_eq!(observe(CHILD, &[]).outcome, ChildOutcome::Unknown);
    assert_eq!(
        observe(CHILD, std::slice::from_ref(&closed)).outcome,
        ChildOutcome::Unknown
    );
    assert_eq!(
        observe(CHILD, std::slice::from_ref(&final_phase)).outcome,
        ChildOutcome::FinalObserved
    );
    assert_eq!(
        observe(CHILD, &[final_phase.clone(), closed.clone()]).outcome,
        ChildOutcome::Succeeded
    );
    let mut bad = closed.clone();
    bad.index += 1;
    assert_eq!(
        observe(CHILD, &[final_phase.clone(), bad]).outcome,
        ChildOutcome::FinalObserved
    );
    for (cancelled, is_error, failed, interrupted, outcome) in [
        (true, false, false, false, ChildOutcome::Cancelled),
        (false, true, false, false, ChildOutcome::Failed),
        (false, false, true, false, ChildOutcome::Failed),
        (false, false, true, true, ChildOutcome::Interrupted),
    ] {
        let mut f = final_phase.clone();
        f.cancelled = Some(cancelled);
        f.is_error = Some(is_error);
        let mut c = closed.clone();
        c.failed = Some(failed);
        c.interrupted = Some(interrupted);
        assert_eq!(observe(CHILD, &[f, c]).outcome, outcome);
    }
}
#[test]
fn stable_identity_overlap_and_terminal_order_are_checked() {
    let mut phases = vec![phase("prepared")];
    assert_eq!(
        merge(&mut phases, phase("prepared")).unwrap(),
        ReplayDisposition::Duplicate
    );
    let mut conflict = phase("prepared");
    conflict.seq += 1;
    assert_eq!(
        merge(&mut phases, conflict).unwrap_err().0,
        ReplyKind::HostUnavailable
    );
    let mut phases = vec![phase("closed")];
    assert_eq!(
        merge(&mut phases, phase("final")).unwrap(),
        ReplayDisposition::OutsideWindow
    );
    assert_eq!(observe(CHILD, &phases).outcome, ChildOutcome::Succeeded);
    let mut later = phase("prepared");
    later.seq = 50;
    later.index = 4;
    assert_eq!(
        merge(&mut phases, later).unwrap_err().0,
        ReplyKind::HostUnavailable
    );
}
#[tokio::test]
async fn merges_real_replay_overlap_without_settling_parent_or_querying_bodies() {
    let mut f = Fixture::new(Mode::Overlap, ready()).await;
    let snapshot = child_snapshot(&f.service, SESSION.into(), request())
        .await
        .unwrap();
    assert_eq!(snapshot.start_seq, 0);
    assert_eq!(snapshot.last_seq, 3);
    assert_eq!(
        snapshot
            .events
            .iter()
            .map(|e| e.disposition)
            .collect::<Vec<_>>(),
        vec![
            ReplayDisposition::Duplicate,
            ReplayDisposition::Applied,
            ReplayDisposition::Applied
        ]
    );
    assert!(!snapshot.resync_required);
    assert_eq!(snapshot.observations[0].outcome, ChildOutcome::Succeeded);
    assert_eq!(snapshot.page.leaf_id.as_deref(), Some("leaf"));
    // The pinned query page remains unchanged; live overlay is separate.
    assert_eq!(snapshot.phases[0].phases.len(), 2);
    assert_eq!(snapshot.phases[0].phases[0].phase, ChildPhaseKind::Prepared);
    let host = f.service.live_session(SESSION).await.unwrap();
    assert!(host.snapshot().await.latest_settlement.is_none());
    assert_eq!(host.snapshot().await.status, "ready");
    let mut types = vec![];
    while let Ok(q) = f.requests.try_recv() {
        types.push(q["type"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        types,
        vec![
            "get_tool_program_children",
            "get_tool_program_child_phases",
            "get_tool_program_children"
        ]
    );
    let subscription = crate::events::Subscription::new(&host, Some(snapshot.last_seq)).await;
    assert!(!subscription.gap);
    assert!(subscription.replay.is_empty());
    f.replies
        .send_projected(json!({"type":"session_entry_committed"}))
        .await;
    let mut stream = std::pin::pin!(crate::events::replay_stream(
        subscription.replay,
        subscription.receiver,
        subscription.after
    ));
    use futures_util::StreamExt;
    assert!(matches!(stream.next().await,Some(crate::events::StreamItem::Event(e)) if e.seq==4));
    drop(host);
    f.close().await;
}
#[tokio::test]
async fn empty_and_sparse_windows_are_bounded_not_completed() {
    for mode in [Mode::Empty, Mode::Sparse] {
        let f = Fixture::new(mode, ready()).await;
        let snapshot = child_snapshot(&f.service, SESSION.into(), request())
            .await
            .unwrap();
        if mode == Mode::Empty {
            assert!(snapshot.observations.is_empty());
        } else {
            assert!(snapshot.phases[0].next_cursor.is_some());
            assert_eq!(snapshot.phases[0].scanned, 128);
            assert_eq!(snapshot.observations[0].outcome, ChildOutcome::Unknown);
        }
        assert!(!snapshot.resync_required);
        f.close().await;
    }
}
#[tokio::test]
async fn old_or_query_only_hosts_receive_no_requests() {
    for ready in [json!({}), {
        let mut r = ready();
        r.as_object_mut().unwrap().remove("toolProgramChildEvents");
        r
    }] {
        let mut f = Fixture::new(Mode::Plain, ready).await;
        assert_eq!(
            child_snapshot(&f.service, SESSION.into(), request())
                .await
                .unwrap_err()
                .0,
            ReplyKind::Unsupported
        );
        assert!(f.requests.try_recv().is_err());
        f.close().await;
    }
}
#[tokio::test]
async fn invalid_snapshot_parameters_fail_before_native_requests() {
    let mut f = Fixture::new(Mode::Plain, ready()).await;
    for n in 0..6 {
        let mut q = request();
        match n {
            0 => q.children.limit = 9,
            1 => q.phase_limit = 0,
            2 => q.phase_limit = 9,
            3 => q.children.direction = Direction::OldestFirst,
            4 => q.children.leaf_id = Some("historical".into()),
            _ => q.children.max_bytes = MAX_CHILD_SNAPSHOT_BYTES + 1,
        }
        assert_eq!(
            child_snapshot(&f.service, SESSION.into(), q)
                .await
                .unwrap_err()
                .0,
            ReplyKind::InvalidInput
        );
    }
    assert!(f.requests.try_recv().is_err());
    f.close().await;
}
#[tokio::test]
async fn raw_replay_eviction_and_selected_metadata_byte_overflow_fail_closed() {
    for mode in [Mode::Evict, Mode::Overflow] {
        let f = Fixture::new(mode, ready()).await;
        assert_eq!(
            child_snapshot(&f.service, SESSION.into(), request())
                .await
                .unwrap_err()
                .0,
            ReplyKind::Conflict
        );
        f.close().await;
    }
}
#[tokio::test]
async fn native_resync_is_explicit_and_foreign_source_is_sanitized() {
    let f = Fixture::new(Mode::Resync, ready()).await;
    let snapshot = child_snapshot(&f.service, SESSION.into(), request())
        .await
        .unwrap();
    assert!(snapshot.resync_required);
    assert_eq!(
        snapshot.events[0].disposition,
        ReplayDisposition::ResyncRequired
    );
    f.close().await;
    let f = Fixture::new(Mode::Foreign, ready()).await;
    let error = child_snapshot(&f.service, SESSION.into(), request())
        .await
        .unwrap_err();
    assert_eq!(error.0, ReplyKind::HostUnavailable);
    assert!(!error.1.to_string().contains("private-wrong-source"));
    f.close().await;
}
#[tokio::test]
async fn unrelated_large_messages_are_not_cloned_into_child_metadata_replay() {
    let f = Fixture::new(Mode::BigGeneric, ready()).await;
    let snapshot = child_snapshot(&f.service, SESSION.into(), request())
        .await
        .unwrap();
    assert_eq!(snapshot.last_seq, 1);
    assert!(snapshot.events.is_empty());
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < 10000);
    f.close().await;
}
#[tokio::test]
async fn dropping_a_snapshot_retires_request_correlation_and_allows_a_fresh_read() {
    let mut f = Fixture::new(Mode::Hold, ready()).await;
    let service = f.service.clone();
    let task =
        tokio::spawn(async move { child_snapshot(&service, SESSION.into(), request()).await });
    let first = f.requests.recv().await.unwrap();
    assert_eq!(first["type"], "get_tool_program_children");
    let pending = f.requests.recv().await.unwrap();
    assert_eq!(pending["type"], "get_tool_program_child_phases");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    *f.mode.lock().unwrap() = Mode::Plain;
    // No raw subscriber remains after cancellation; the retired correlation
    // must not consume this response or settle the next snapshot.
    let _ = f.replies.send(json!({"type":"response","id":pending["id"],"success":false,"error":"late-cancelled-request"}));
    let snapshot = child_snapshot(&f.service, SESSION.into(), request())
        .await
        .unwrap();
    assert_eq!(snapshot.observations[0].outcome, ChildOutcome::Unknown);
    f.close().await;
}

#[test]
fn identities_span_all_child_pages_and_live_overlap_not_just_each_child() {
    let a: query::ChildSummary = serde_json::from_value(summary()).unwrap();
    let mut b = a.clone();
    b.child_id = "tpchild-77777777-7777-4777-8777-777777777777".into();
    b.seq = 20;
    let mut identities = Identities::default();
    identities.begin(&a).unwrap();
    identities.begin(&b).unwrap();
    // A requested-phase replay cannot reuse B's begin sequence.
    let mut replay = phase("requested");
    replay.seq = 20;
    assert_eq!(
        identities.phase(&replay).unwrap_err().0,
        ReplyKind::HostUnavailable
    );
    let mut page_a = phase("prepared");
    page_a.seq = 25;
    identities.phase(&page_a).unwrap();
    // A second child's otherwise valid page cannot reuse A's phase entry ID.
    let mut page_b = page_a.clone();
    page_b.child_id = b.child_id.clone();
    page_b.seq = 30;
    page_b.descriptor.as_mut().unwrap().child_id = b.child_id.clone();
    query::phase(&page_b, &b.child_id).unwrap();
    assert_eq!(
        identities.phase(&page_b).unwrap_err().0,
        ReplyKind::HostUnavailable
    );
    // Legitimate cross-child interleaving and exact overlaps remain accepted.
    page_b.entry_id = "tpphase-88888888-8888-4888-8888-888888888888".into();
    identities.phase(&page_b).unwrap();
    identities.phase(&page_a).unwrap();
    identities.begin(&a).unwrap();
    let mut historical = a.clone();
    historical.origin_verification = query::ChildOriginVerification::ForkCopy;
    identities.begin(&historical).unwrap();
}

#[tokio::test]
async fn replacing_the_same_session_id_during_a_native_page_requires_resync() {
    let mut f = Fixture::new(Mode::Hold, ready()).await;
    let g = Fixture::new(Mode::Plain, ready()).await;
    let original = f.service.live_session(SESSION).await.unwrap();
    let replacement = g.service.live_session(SESSION).await.unwrap();
    let service = f.service.clone();
    let read =
        tokio::spawn(async move { child_snapshot(&service, SESSION.into(), request()).await });
    f.requests.recv().await.unwrap();
    let pending = f.requests.recv().await.unwrap();
    f.service
        .sessions
        .write()
        .await
        .insert(SESSION.into(), replacement);
    f.replies.send(json!({"type":"response","id":pending["id"],"success":true,"data":{
        "leafId":"leaf","childId":CHILD,"phases":[],"nextCursor":null,"scanned":0,"scanExhausted":true,"state":"unknown"}})).unwrap();
    assert_eq!(read.await.unwrap().unwrap_err().0, ReplyKind::Conflict);
    f.service
        .sessions
        .write()
        .await
        .insert(SESSION.into(), original);
    f.close().await;
    g.close().await;
}

#[tokio::test(start_paused = true)]
async fn total_snapshot_deadline_covers_a_stalled_native_read() {
    let f = Fixture::new(Mode::Hold, ready()).await;
    let started = tokio::time::Instant::now();
    let error = child_snapshot(&f.service, SESSION.into(), request())
        .await
        .unwrap_err();
    // The existing per-RPC deadline can win the same 30-second boundary;
    // either explicit error is valid, but no extra per-child wait is allowed.
    assert!(matches!(
        error.0,
        ReplyKind::Timeout | ReplyKind::HostUnavailable
    ));
    assert!(started.elapsed() <= Duration::from_secs(30));
    *f.mode.lock().unwrap() = Mode::Plain;
    child_snapshot(&f.service, SESSION.into(), request())
        .await
        .unwrap();
    f.close().await;
}
