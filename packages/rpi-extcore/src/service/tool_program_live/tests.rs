use super::*;
use crate::{host::TestEvents, service::BackendConfig};
use query::{ChildPageQuery, Direction};
use serde_json::json;
use snapshot::ChildOutcome;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};

const CHILD: &str = "tpchild-11111111-1111-4111-8111-111111111111";
const OTHER: &str = "tpchild-22222222-2222-4222-8222-222222222222";
fn ready() -> Value {
    json!({"sessionId":"session","toolProgramChildren":{"version":1,"summaryCommand":"get_tool_program_children",
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
fn summary(id: &str, seq: u64) -> Value {
    json!({"version":1,"childId":id,"seq":seq,"origin":{"runId":"run","assistantEntryId":"assistant","toolIndex":0},
        "originVerification":"forkCopy","rootCallId":"call","rootToolName":"program","toolName":"read",
        "ordinal":0,"parentChildId":null,"state":"unknown"})
}
fn phase(index: u64, kind: &str) -> ChildPhase {
    let body = "66666666-6666-4666-8666-666666666666";
    serde_json::from_value(json!({"entryId":format!("tpphase-{index:08x}-3333-4333-8333-333333333333"),
        "seq":11+index,"version":1,"childId":CHILD,"index":index,"phase":kind,
        "descriptor":if kind=="closed" {Value::Null} else {json!({"version":1,"bodyId":body,
            "descriptorId":format!("tpbody-{body}-commit"),"childId":CHILD,"kind":kind,"codec":"native-json-v1",
            "encoding":"base64","byteLength":2,"chunkBytes":32768,"chunks":1,"sha256":"a".repeat(64)})},
        "isError":(kind=="final").then_some(false),"cancelled":(kind=="final").then_some(false),
        "failed":(kind=="closed").then_some(false),"interrupted":(kind=="closed").then_some(false)})).unwrap()
}
fn event(p: &ChildPhase) -> Value {
    let mut data = serde_json::to_value(p).unwrap();
    data.as_object_mut().unwrap().remove("entryId");
    data.as_object_mut().unwrap().remove("seq");
    json!({"type":"tool_program_child_phase","schemaVersion":1,"sourceSessionId":"session","lane":"main",
        "entryId":p.entry_id,"parentEntryId":"parent","seq":p.seq,"customType":"rpi.tool_program.child_phase.v1","data":data})
}
fn begin(id: &str, seq: u64) -> Value {
    let mut data = summary(id, seq);
    for key in ["seq", "originVerification", "state"] {
        data.as_object_mut().unwrap().remove(key);
    }
    json!({"type":"tool_program_child_begin","schemaVersion":1,"sourceSessionId":"session","lane":"main",
        "entryId":id,"parentEntryId":"parent","seq":seq,"customType":"rpi.tool_program.child_begin.v1","data":data})
}
// The projected event and authoritative query oracle share genuine metadata,
// but tests can silently navigate the native branch without publishing an event.
struct BranchOracle {
    leaf: String,
    head: Value,
    others: Vec<Value>,
    phases: HashMap<String, Vec<ChildPhase>>,
    snapshot_phases: HashMap<String, Vec<ChildPhase>>,
    reads: Vec<Value>,
    hold_at: Option<usize>,
    fail_next: bool,
    sparse: bool,
}
#[derive(Clone)]
struct BranchEvents {
    inner: TestEvents,
    oracle: Arc<std::sync::Mutex<BranchOracle>>,
}
impl BranchEvents {
    async fn send_projected(&self, value: Value) {
        if let Ok(Some(ChildEvent::Phase(commit))) = events::parse(
            &ready(),
            &ChildEventContext {
                session_id: "session".into(),
                lane: "main".into(),
            },
            &value,
        ) {
            let mut oracle = self.oracle.lock().unwrap();
            let phases = oracle
                .phases
                .entry(commit.data.child_id.clone())
                .or_default();
            let phase = commit.phase();
            if !phases.contains(&phase) {
                phases.push(phase);
                phases.sort_by_key(|p| std::cmp::Reverse(p.seq));
                phases.truncate(32);
            }
        }
        self.inner.send_projected(value).await;
    }
    fn read_count(&self) -> usize {
        self.oracle.lock().unwrap().reads.len()
    }
}
struct Fixture {
    service: Arc<Service>,
    host: Arc<HostSession>,
    publish: BranchEvents,
    task: tokio::task::JoinHandle<()>,
    root: PathBuf,
    phase_seen: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl Fixture {
    async fn new(
        capacity: usize,
        overlap: Vec<Value>,
        hold: bool,
        terminal: bool,
        two_children: bool,
        ready: Value,
    ) -> Self {
        let root = std::env::temp_dir().join(format!("rpi-child-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let service = Service::new(BackendConfig {
            project: root.clone(),
            registry: root.join("registry.json"),
            rpi_bin: root.join("must-not-launch"),
            ..Default::default()
        })
        .unwrap();
        let bare = BareFixture::new(capacity, overlap, hold, terminal, two_children, ready).await;
        service
            .sessions
            .write()
            .await
            .insert("session".into(), bare.host.clone());
        let (host, publish, task, phase_seen, release) = (
            bare.host,
            bare.publish,
            bare.task,
            bare.phase_seen,
            bare.release,
        );
        Self {
            service,
            host,
            publish,
            task,
            root,
            phase_seen,
            release,
        }
    }
    async fn plain() -> Self {
        Self::new(1024, vec![], false, false, false, ready()).await
    }
    async fn subscribe(&self) -> ChildSubscription {
        child_subscription(&self.service, "session".into(), request())
            .await
            .unwrap()
    }
    async fn close(self) {
        self.task.abort();
        let _ = self.task.await;
        self.service.shutdown().await;
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
struct BareFixture {
    host: Arc<HostSession>,
    publish: BranchEvents,
    task: tokio::task::JoinHandle<()>,
    phase_seen: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl BareFixture {
    async fn new(
        capacity: usize,
        overlap: Vec<Value>,
        hold: bool,
        terminal: bool,
        two_children: bool,
        ready: Value,
    ) -> Self {
        let (writer, peer) = tokio::io::duplex(8192);
        let (host, publish) = HostSession::test_connection_with_ready(writer, capacity, ready);
        let initial = if terminal {
            vec![phase(3, "closed"), phase(2, "final")]
        } else {
            vec![phase(1, "prepared"), phase(0, "requested")]
        };
        let oracle = Arc::new(std::sync::Mutex::new(BranchOracle {
            leaf: "leaf".into(),
            head: summary(CHILD, 10),
            others: if two_children {
                vec![summary(OTHER, 9)]
            } else {
                vec![]
            },
            phases: HashMap::from([(CHILD.into(), initial.clone())]),
            snapshot_phases: HashMap::from([(CHILD.into(), initial)]),
            reads: Vec::new(),
            hold_at: None,
            fail_next: false,
            sparse: false,
        }));
        let publish = BranchEvents {
            inner: publish,
            oracle,
        };
        let sender = publish.clone();
        let phase_seen = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let (seen, gate) = (phase_seen.clone(), release.clone());
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let q: Value = serde_json::from_str(&line).unwrap();
                let (hold_read, fail) = {
                    let mut oracle = sender.oracle.lock().unwrap();
                    oracle.reads.push(q.clone());
                    (
                        oracle.hold_at == Some(oracle.reads.len()),
                        std::mem::take(&mut oracle.fail_next),
                    )
                };
                if hold_read {
                    seen.notify_one();
                    gate.notified().await;
                }
                if fail {
                    sender
                        .inner
                        .send(json!({"type":"response","id":q["id"],"success":false,
                        "error":"SECRET native branch unavailable"}))
                        .unwrap();
                    continue;
                }
                let data = match q["type"].as_str().unwrap() {
                    "get_tool_program_children" => {
                        let oracle = sender.oracle.lock().unwrap();
                        // Explicit leaves remain pinned for the original snapshot.
                        let leaf = q["leafId"].as_str().unwrap_or(&oracle.leaf);
                        let mut children = vec![oracle.head.clone()];
                        if q["limit"] != 1 {
                            children.extend(oracle.others.clone());
                        }
                        if children.len() >= q["limit"].as_u64().unwrap() as usize {
                            let last = children.last().unwrap();
                            json!({"leafId":leaf,"children":children,"scanExhausted":false,
                                "nextCursor":{"sessionId":"session","lane":"main","leafId":leaf,"direction":"newestFirst",
                                "childId":null,"afterEntryId":last["childId"],"afterSeq":last["seq"]}})
                        } else {
                            json!({"leafId":leaf,"children":children,"nextCursor":null,"scanExhausted":true})
                        }
                    }
                    "get_tool_program_child_phases" => {
                        if q["limit"] == 2 {
                            seen.notify_one();
                            if hold {
                                gate.notified().await;
                            }
                            for value in &overlap {
                                sender.send_projected(value.clone()).await;
                            }
                        }
                        let oracle = sender.oracle.lock().unwrap();
                        let phases = if q["limit"] == 2 {
                            oracle
                                .snapshot_phases
                                .get(q["childId"].as_str().unwrap())
                                .cloned()
                                .unwrap_or_default()
                        } else if oracle.sparse {
                            vec![]
                        } else {
                            oracle
                                .phases
                                .get(q["childId"].as_str().unwrap())
                                .cloned()
                                .unwrap_or_default()
                        };
                        let leaf = q["leafId"].as_str().unwrap();
                        if oracle.sparse && q["limit"] == 32 {
                            json!({"leafId":leaf,"childId":q["childId"],"phases":phases,"scanned":128,
                                "nextCursor":{"sessionId":"session","lane":"main","leafId":leaf,"direction":"newestFirst",
                                "childId":q["childId"],"afterEntryId":phase(0,"requested").entry_id,"afterSeq":11},
                                "scanExhausted":false,"state":"unknown"})
                        } else {
                            json!({"leafId":leaf,"childId":q["childId"],"scanned":phases.len(),"phases":phases,
                                "nextCursor":null,"scanExhausted":true,"state":"unknown"})
                        }
                    }
                    other => panic!("unexpected execution/body/fallback request: {other}"),
                };
                // Dropping a query future removes its correlation. A late reply
                // is legitimate and must not kill this authoritative native peer.
                let _ = sender
                    .inner
                    .send(json!({"type":"response","id":q["id"],"success":true,"data":data}));
            }
        });
        Self {
            host,
            publish,
            task,
            phase_seen,
            release,
        }
    }
    async fn plain() -> Self {
        let mut profile = ready();
        profile["sessionId"] = json!("session");
        Self::new(1024, vec![], false, false, false, profile).await
    }
    async fn close(self) {
        self.host.shutdown().await;
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn reason(s: &mut ChildSubscription, expected: ChildResyncReason) {
    let item = tokio::time::timeout(std::time::Duration::from_secs(2), s.next())
        .await
        .unwrap()
        .unwrap();
    let wire = serde_json::to_value(&item).unwrap();
    assert_eq!(wire["type"], "resyncRequired");
    let ChildStreamItem::ResyncRequired { reason, .. } = item else {
        panic!("expected resync: {wire}")
    };
    assert_eq!(reason, expected);
    assert!(s.next().await.is_none());
}
async fn update(
    s: &mut ChildSubscription,
    expected: ReplayDisposition,
) -> Option<ChildObservation> {
    let item = tokio::time::timeout(std::time::Duration::from_secs(2), s.next())
        .await
        .unwrap()
        .unwrap();
    let ChildStreamItem::Update {
        disposition,
        observation,
        ..
    } = item
    else {
        panic!("{item:?}")
    };
    assert_eq!(disposition, expected);
    observation.map(|value| *value)
}

#[tokio::test]
async fn subscribe_before_snapshot_skip_overlap_then_final_close_without_parent_settlement() {
    let f = Fixture::new(
        1024,
        vec![event(&phase(1, "prepared"))],
        false,
        false,
        false,
        ready(),
    )
    .await;
    let mut s = f.subscribe().await;
    assert_eq!(s.snapshot().last_seq, 1);
    let pinned = serde_json::to_value(s.snapshot()).unwrap();
    f.publish.send_projected(event(&phase(2, "final"))).await;
    assert_eq!(
        update(&mut s, ReplayDisposition::Applied)
            .await
            .unwrap()
            .outcome,
        ChildOutcome::FinalObserved
    );
    f.publish.send_projected(event(&phase(3, "closed"))).await;
    assert_eq!(
        update(&mut s, ReplayDisposition::Applied)
            .await
            .unwrap()
            .outcome,
        ChildOutcome::Succeeded
    );
    assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), pinned);
    assert_eq!(
        s.snapshot.page.children[0].origin_verification,
        query::ChildOriginVerification::ForkCopy
    );
    assert!(f.host.snapshot().await.latest_settlement.is_none());
    drop(s);
    f.close().await;
}
#[tokio::test]
async fn seed_typed_overlap_terminal_facts_and_reject_post_closed_update() {
    let f = Fixture::new(
        1024,
        vec![event(&phase(2, "final")), event(&phase(3, "closed"))],
        false,
        false,
        false,
        ready(),
    )
    .await;
    let mut s = f.subscribe().await;
    assert_eq!(
        snapshot::observe(CHILD, &s.working[CHILD]).outcome,
        ChildOutcome::Succeeded
    );
    f.publish.send_projected(event(&phase(4, "update"))).await;
    reason(&mut s, ChildResyncReason::IdentityConflict).await;
    f.close().await;
}
#[tokio::test]
async fn terminal_query_facts_preserved_and_same_existing_seq_duplicates_are_idempotent() {
    let f = Fixture::new(1024, vec![], false, true, false, ready()).await;
    let mut s = f.subscribe().await;
    for value in [
        json!({"type":"tool_execution_update","body":"secret"}),
        event(&phase(2, "final")),
        event(&phase(2, "final")),
    ] {
        f.publish.send_projected(value).await;
    }
    assert!(update(&mut s, ReplayDisposition::Duplicate).await.is_none());
    assert!(update(&mut s, ReplayDisposition::Duplicate).await.is_none());
    assert_eq!(s.cursor, 3);
    assert_eq!(
        snapshot::observe(CHILD, &s.working[CHILD]).outcome,
        ChildOutcome::Succeeded
    );
    f.close().await;
}
#[tokio::test]
async fn interleaved_nonchild_advances_cursor_without_forwarding_body() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    f.publish
        .send_projected(json!({"type":"tool_execution_update","body":"secret"}))
        .await;
    f.publish.send_projected(event(&phase(2, "final"))).await;
    let item = s.next().await.unwrap();
    assert!(matches!(item, ChildStreamItem::Update { seq: 2, .. }));
    assert!(!serde_json::to_string(&item).unwrap().contains("secret"));
    f.close().await;
}
#[tokio::test]
async fn native_phase_gap_is_terminal_not_inferred_completion() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    f.publish.send_projected(event(&phase(4, "final"))).await;
    reason(&mut s, ChildResyncReason::PhaseDiscontinuity).await;
    f.close().await;
}
#[tokio::test]
async fn local_core_gap_is_terminal() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    // Real publish, then simulate a discontinuous consumer boundary without
    // adding a host test-only API or editing host/event ownership.
    f.publish.send_projected(event(&phase(2, "final"))).await;
    s.cursor = 7;
    reason(&mut s, ChildResyncReason::CoreSequenceGap).await;
    f.close().await;
}
#[tokio::test]
async fn lagged_receiver_during_snapshot_is_explicit_even_when_snapshot_is_valid() {
    let overlap = (0..5)
        .map(|_| json!({"type":"session_entry_committed"}))
        .collect();
    let f = Fixture::new(2, overlap, false, false, false, ready()).await;
    let mut s = f.subscribe().await;
    assert_eq!(s.snapshot.last_seq, 5);
    reason(&mut s, ChildResyncReason::Lagged).await;
    f.close().await;
}
#[tokio::test]
async fn live_receiver_lag_is_terminal() {
    let f = Fixture::new(2, vec![], false, false, false, ready()).await;
    let mut s = f.subscribe().await;
    for _ in 0..5 {
        f.publish
            .send_projected(json!({"type":"session_entry_committed"}))
            .await;
    }
    reason(&mut s, ChildResyncReason::Lagged).await;
    f.close().await;
}
#[tokio::test]
async fn malformed_foreign_lane_and_unknown_events_are_sanitized() {
    for field in ["sourceSessionId", "lane", "customType", "type", "body"] {
        let f = Fixture::plain().await;
        let mut s = f.subscribe().await;
        let mut value = event(&phase(2, "final"));
        value[field] = json!(if field == "type" {
            "tool_program_child_unknown"
        } else {
            "private-secret"
        });
        f.publish.send_projected(value).await;
        let item = s.next().await.unwrap();
        assert!(matches!(
            item,
            ChildStreamItem::ResyncRequired {
                reason: ChildResyncReason::InvalidChildEvent,
                ..
            }
        ));
        assert!(!serde_json::to_string(&item)
            .unwrap()
            .contains("private-secret"));
        assert!(s.next().await.is_none());
        f.close().await;
    }
}
#[tokio::test]
async fn native_resync_and_new_child_stop_without_refresh() {
    for (value, expected) in [
        (
            json!({"type":"tool_program_child_resync_required","reason":"commit_stream_lagged","skipped":1}),
            ChildResyncReason::NativeResync,
        ),
        (begin(OTHER, 20), ChildResyncReason::OutsideWindow),
    ] {
        let f = Fixture::plain().await;
        let mut s = f.subscribe().await;
        f.publish.send_projected(value).await;
        reason(&mut s, expected).await;
        f.close().await;
    }
}
#[tokio::test]
async fn uncertain_snapshot_yields_resync_first() {
    let f = Fixture::new(1024, vec![begin(OTHER, 20)], false, false, false, ready()).await;
    let mut s = f.subscribe().await;
    assert!(s.snapshot().resync_required);
    reason(&mut s, ChildResyncReason::SnapshotUncertain).await;
    f.close().await;
}
#[tokio::test]
async fn same_id_host_replacement_never_mixes_feeds() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    let (writer, _peer) = tokio::io::duplex(1024);
    let (replacement, _events) = HostSession::test_connection_with_ready(writer, 16, ready());
    f.service
        .sessions
        .write()
        .await
        .insert("session".into(), replacement.clone());
    f.publish.send_projected(event(&phase(2, "final"))).await;
    reason(&mut s, ChildResyncReason::HostReplaced).await;
    f.host.shutdown().await;
    f.close().await;
}
#[tokio::test]
async fn replacement_during_begin_is_rejected() {
    let f = Fixture::new(1024, vec![], true, false, false, ready()).await;
    let state = f.service.clone();
    let attach =
        tokio::spawn(async move { child_subscription(&state, "session".into(), request()).await });
    f.phase_seen.notified().await;
    let (writer, _peer) = tokio::io::duplex(1024);
    let (replacement, _events) = HostSession::test_connection_with_ready(writer, 16, ready());
    f.service
        .sessions
        .write()
        .await
        .insert("session".into(), replacement);
    f.release.notify_one();
    assert!(matches!(
        attach.await.unwrap(),
        Err((ReplyKind::Conflict, _))
    ));
    f.host.shutdown().await;
    f.close().await;
}
#[tokio::test]
async fn close_and_error_are_explicit_terminal_resync() {
    for (kind, expected) in [
        ("host_closed", ChildResyncReason::HostClosed),
        ("host_error", ChildResyncReason::HostError),
    ] {
        let f = Fixture::plain().await;
        let mut s = f.subscribe().await;
        f.publish.send_projected(json!({"type":kind})).await;
        reason(&mut s, expected).await;
        f.close().await;
    }
}
#[tokio::test]
async fn drop_releases_only_subscription_and_cancelled_next_loses_no_event() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    {
        let next = s.next();
        tokio::pin!(next);
        assert!(futures_util::poll!(&mut next).is_pending());
    }
    f.publish.send_projected(event(&phase(2, "final"))).await;
    update(&mut s, ReplayDisposition::Applied).await;
    drop(s);
    assert!(f.service.live_session("session").await.is_some());
    assert_ne!(f.host.snapshot().await.status, "closed");
    let mut again = f.subscribe().await;
    assert_eq!(again.snapshot().last_seq, 1);
    f.publish.send_projected(event(&phase(2, "final"))).await;
    update(&mut again, ReplayDisposition::Applied).await;
    f.close().await;
}
#[tokio::test]
async fn next_cancel_after_receive_retains_frame_for_post_receive_host_check() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    {
        let next = s.next();
        tokio::pin!(next);
        assert!(futures_util::poll!(&mut next).is_pending());
        let registry = f.service.registry.write().await;
        f.publish.send_projected(event(&phase(2, "final"))).await;
        assert!(futures_util::poll!(&mut next).is_pending());
        drop(registry);
    }
    assert!(s.pending.is_some());
    update(&mut s, ReplayDisposition::Applied).await;
    f.close().await;
}
#[tokio::test]
async fn long_stream_bounded_overlays_and_identity_history() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    for index in 2..302 {
        f.publish
            .send_projected(event(&phase(index, "update")))
            .await;
        update(&mut s, ReplayDisposition::Applied).await;
        assert!(s.working.len() <= 8);
        assert!(s.working.values().all(|p| p.len() <= 8));
        assert!(s.envelopes.len() <= RECENT_IDENTITIES);
    }
    assert!(!s.identities.contains(&phase(2, "update").entry_id));
    assert!(s.identities.contains(&phase(301, "update").entry_id));
    f.publish.send_projected(event(&phase(302, "final"))).await;
    update(&mut s, ReplayDisposition::Applied).await;
    f.publish.send_projected(event(&phase(303, "closed"))).await;
    assert_eq!(
        update(&mut s, ReplayDisposition::Applied)
            .await
            .unwrap()
            .outcome,
        ChildOutcome::Succeeded
    );
    f.publish.send_projected(event(&phase(2, "update"))).await;
    reason(&mut s, ChildResyncReason::UnknownOldIdentity).await;
    f.close().await;
}
#[tokio::test]
async fn cross_child_entry_id_or_sequence_conflicts_fail_closed() {
    for same_id in [true, false] {
        let f = Fixture::new(1024, vec![], false, false, true, ready()).await;
        let mut s = f.subscribe().await;
        let mut p = phase(2, "final");
        if same_id {
            p.entry_id = phase(1, "prepared").entry_id;
        } else {
            p.seq = phase(1, "prepared").seq;
        }
        p.child_id = OTHER.into();
        p.descriptor.as_mut().unwrap().child_id = OTHER.into();
        f.publish.send_projected(event(&p)).await;
        let item = s.next().await.unwrap();
        assert!(matches!(
            item,
            ChildStreamItem::ResyncRequired {
                reason: ChildResyncReason::IdentityConflict | ChildResyncReason::UnknownOldIdentity,
                ..
            }
        ));
        assert!(s.next().await.is_none());
        f.close().await;
    }
}
#[tokio::test]
async fn envelope_contradiction_is_not_hidden_by_phase_deduplication() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    let p = event(&phase(2, "final"));
    f.publish.send_projected(p.clone()).await;
    update(&mut s, ReplayDisposition::Applied).await;
    let mut altered = p;
    altered["parentEntryId"] = json!("other-parent");
    f.publish.send_projected(altered).await;
    reason(&mut s, ChildResyncReason::IdentityConflict).await;
    f.close().await;
}
#[tokio::test]
async fn independent_capability_validation_precedes_queries() {
    for profile in ["toolProgramChildren", "toolProgramChildEvents"] {
        let mut r = ready();
        r.as_object_mut().unwrap().remove(profile);
        let f = Fixture::new(16, vec![], false, false, false, r).await;
        assert!(matches!(
            child_subscription(&f.service, "session".into(), request()).await,
            Err((ReplyKind::Unsupported, _))
        ));
        f.close().await;
    }
}

#[tokio::test]
async fn replacement_after_receive_is_detected_before_forwarding() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    {
        let next = s.next();
        tokio::pin!(next);
        assert!(futures_util::poll!(&mut next).is_pending());
        let registry = f.service.registry.write().await;
        f.publish.send_projected(event(&phase(2, "final"))).await;
        assert!(futures_util::poll!(&mut next).is_pending());
        let (writer, _peer) = tokio::io::duplex(1024);
        let (replacement, _events) = HostSession::test_connection_with_ready(writer, 16, ready());
        f.service
            .sessions
            .write()
            .await
            .insert("session".into(), replacement);
        drop(registry);
    }
    reason(&mut s, ChildResyncReason::HostReplaced).await;
    f.host.shutdown().await;
    f.close().await;
}

#[tokio::test]
async fn actual_shared_host_shutdown_notifies_subscription() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    f.host.shutdown().await;
    reason(&mut s, ChildResyncReason::HostClosed).await;
    f.close().await;
}

#[tokio::test]
async fn bare_host_subscription_final_close_and_drop_preserve_shared_host() {
    let f = BareFixture::plain().await;
    let mut s = child_subscription_for_host(f.host.clone(), request())
        .await
        .unwrap();
    f.publish.send_projected(event(&phase(2, "final"))).await;
    let observation = update(&mut s, ReplayDisposition::Applied).await;
    assert_eq!(observation.unwrap().outcome, ChildOutcome::FinalObserved);
    f.publish.send_projected(event(&phase(3, "closed"))).await;
    let observation = update(&mut s, ReplayDisposition::Applied).await;
    assert_eq!(observation.unwrap().outcome, ChildOutcome::Succeeded);
    drop(s);
    assert_ne!(f.host.snapshot().await.status, "closed");
    f.close().await;
}
#[tokio::test]
async fn bare_host_subscription_actual_shutdown_and_error_keep_typed_causes() {
    for error in [false, true] {
        let f = BareFixture::plain().await;
        let mut s = child_subscription_for_host(f.host.clone(), request())
            .await
            .unwrap();
        if error {
            f.publish
                .send_projected(json!({"type":"host_error","error":"SECRET"}))
                .await;
        } else {
            f.host.shutdown().await;
        }
        reason(
            &mut s,
            if error {
                ChildResyncReason::HostError
            } else {
                ChildResyncReason::HostClosed
            },
        )
        .await;
        assert!(s.next().await.is_none());
        f.close().await;
    }
}
#[tokio::test]
async fn bare_host_subscription_profile_preflight_and_closed_attachment() {
    for missing in ["toolProgramChildren", "toolProgramChildEvents"] {
        let mut profile = ready();
        profile["sessionId"] = json!("session");
        profile.as_object_mut().unwrap().remove(missing);
        let f = BareFixture::new(16, vec![], false, false, false, profile).await;
        assert!(matches!(
            child_subscription_for_host(f.host.clone(), request()).await,
            Err((ReplyKind::Unsupported, _))
        ));
        assert!(futures_util::FutureExt::now_or_never(f.phase_seen.notified()).is_none());
        f.close().await;
    }
    let f = BareFixture::plain().await;
    let mut q = request();
    q.children.limit = 9;
    assert!(matches!(
        child_subscription_for_host(f.host.clone(), q).await,
        Err((ReplyKind::InvalidInput, _))
    ));
    f.host.shutdown().await;
    assert!(matches!(
        child_subscription_for_host(f.host.clone(), request()).await,
        Err((ReplyKind::HostUnavailable, _))
    ));
    f.close().await;
}
#[tokio::test]
async fn bare_host_subscription_retains_only_existing_arc_and_cancelled_next() {
    let f = BareFixture::plain().await;
    let mut s = child_subscription_for_host(f.host.clone(), request())
        .await
        .unwrap();
    let weak = Arc::downgrade(&f.host);
    let BareFixture {
        host,
        publish,
        task,
        ..
    } = f;
    drop(host);
    assert!(weak.upgrade().is_some());
    {
        let next = s.next();
        tokio::pin!(next);
        assert!(futures_util::poll!(&mut next).is_pending());
    }
    publish.send_projected(event(&phase(2, "final"))).await;
    update(&mut s, ReplayDisposition::Applied).await;
    let with_subscription = weak.strong_count();
    drop(s);
    // The publishing fixture and its peer task also own the shared host.
    assert_eq!(weak.strong_count() + 1, with_subscription);
    drop(publish);
    task.abort();
    let _ = task.await;
    assert!(weak.upgrade().is_none());
}

fn hold_proof_read(f: &Fixture, offset: usize) {
    // Discard the snapshot-phase notification before arming the ongoing gate.
    let _ = futures_util::FutureExt::now_or_never(f.phase_seen.notified());
    let mut oracle = f.publish.oracle.lock().unwrap();
    oracle.hold_at = Some(oracle.reads.len() + offset);
}

#[tokio::test]
async fn silent_fork_after_begin_missing_old_phase_never_forwards_valid_new_phase() {
    // Even an unchanged leaf/head cannot substitute for retained-phase ancestry.
    for changed_leaf in [false, true] {
        let f = Fixture::plain().await;
        let mut s = f.subscribe().await;
        {
            let mut oracle = f.publish.oracle.lock().unwrap();
            if changed_leaf {
                oracle.leaf = "branch-B".into();
            }
            oracle
                .phases
                .get_mut(CHILD)
                .unwrap()
                .retain(|p| p.index == 0);
        }
        let before = f.publish.read_count();
        f.publish.send_projected(event(&phase(2, "final"))).await;
        reason(&mut s, ChildResyncReason::BranchChanged).await;
        assert_eq!(f.publish.read_count() - before, 2);
        assert_eq!(s.cursor, s.snapshot.last_seq);
        assert_eq!(
            snapshot::observe(CHILD, &s.working[CHILD]).outcome,
            ChildOutcome::Unknown
        );
        f.close().await;
    }
}

#[tokio::test]
async fn duplicate_envelopes_and_begins_still_require_authoritative_proof() {
    for duplicate_begin in [false, true] {
        let f = Fixture::plain().await;
        let mut s = f.subscribe().await;
        let value = if duplicate_begin {
            begin(CHILD, 10)
        } else {
            event(&phase(2, "final"))
        };
        f.publish.send_projected(value.clone()).await;
        update(
            &mut s,
            if duplicate_begin {
                ReplayDisposition::Duplicate
            } else {
                ReplayDisposition::Applied
            },
        )
        .await;
        f.publish
            .oracle
            .lock()
            .unwrap()
            .phases
            .get_mut(CHILD)
            .unwrap()
            .retain(|p| p.index != 1);
        f.publish.send_projected(value).await;
        reason(&mut s, ChildResyncReason::BranchChanged).await;
        f.close().await;
    }
}

#[tokio::test]
async fn fresh_leaf_with_all_facts_and_same_leaf_exact_duplicates_are_valid() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    let pinned = serde_json::to_value(s.snapshot()).unwrap();
    // A normal append changes leaf; it is not itself a branch-generation token.
    f.publish.oracle.lock().unwrap().leaf = "append-leaf".into();
    for expected in [ReplayDisposition::Applied, ReplayDisposition::Duplicate] {
        let before = f.publish.read_count();
        f.publish.send_projected(event(&phase(2, "final"))).await;
        update(&mut s, expected).await;
        assert_eq!(f.publish.read_count() - before, 3);
    }
    {
        let oracle = f.publish.oracle.lock().unwrap();
        for q in &oracle.reads[3..] {
            assert_eq!(q["maxBytes"], 32768);
            assert_eq!(q["direction"], "newestFirst");
            assert!(q["cursor"].is_null());
            if q["type"] == "get_tool_program_children" {
                assert_eq!(q["limit"], 1);
                assert!(q["leafId"].is_null());
            } else {
                assert_eq!(q["limit"], 32);
                assert_eq!(q["leafId"], "append-leaf");
            }
        }
    }
    assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), pinned);
    f.close().await;
}

#[tokio::test]
async fn branch_changes_between_proof_reads_are_terminal_even_with_all_facts() {
    for change_head in [false, true] {
        let f = Fixture::plain().await;
        let mut s = f.subscribe().await;
        hold_proof_read(&f, 2); // first fresh head already read; phase scan pending
        f.publish.send_projected(event(&phase(2, "final"))).await;
        {
            let next = s.next();
            tokio::pin!(next);
            tokio::select! {
                _ = f.phase_seen.notified() => {},
                item = &mut next => panic!("delivered before proof: {item:?}"),
            }
            {
                let mut oracle = f.publish.oracle.lock().unwrap();
                if change_head {
                    oracle.head = summary(OTHER, 20);
                } else {
                    oracle.leaf = "concurrent-append-or-navigation".into();
                }
            }
            f.release.notify_one();
            assert!(matches!(
                next.await,
                Some(ChildStreamItem::ResyncRequired {
                    reason: ChildResyncReason::BranchChanged,
                    ..
                })
            ));
        }
        assert!(s.next().await.is_none());
        assert_eq!(s.cursor, s.snapshot.last_seq);
        f.close().await;
    }
}

#[tokio::test]
async fn cancelling_pending_proof_retains_frame_and_all_working_state() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    let working = s.working.clone();
    let cursor = s.cursor;
    let native_high = s.native_high;
    let envelopes = s.envelopes.clone();
    hold_proof_read(&f, 2);
    let value = event(&phase(2, "final"));
    f.publish.send_projected(value.clone()).await;
    {
        let next = s.next();
        tokio::pin!(next);
        tokio::select! {
            _ = f.phase_seen.notified() => {},
            item = &mut next => panic!("delivered before proof: {item:?}"),
        }
    }
    assert_eq!(s.pending.as_ref().unwrap().value, value);
    assert_eq!(s.working, working);
    assert_eq!(s.cursor, cursor);
    assert_eq!(s.native_high, native_high);
    assert_eq!(s.envelopes, envelopes);
    assert!(!s.identities.contains(&phase(2, "final").entry_id));
    let reads = f.publish.read_count();
    f.release.notify_one();
    update(&mut s, ReplayDisposition::Applied).await;
    assert_eq!(f.publish.read_count() - reads, 3); // redo BOTH heads + phases
    assert!(s.pending.is_none());
    f.close().await;
}

#[tokio::test]
async fn host_replacement_between_proof_reads_never_forwards() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    hold_proof_read(&f, 2);
    f.publish.send_projected(event(&phase(2, "final"))).await;
    {
        let next = s.next();
        tokio::pin!(next);
        tokio::select! {
            _ = f.phase_seen.notified() => {},
            item = &mut next => panic!("delivered before proof: {item:?}"),
        }
        let (writer, _peer) = tokio::io::duplex(1024);
        let (replacement, _) = HostSession::test_connection_with_ready(writer, 16, ready());
        f.service
            .sessions
            .write()
            .await
            .insert("session".into(), replacement);
        f.release.notify_one();
        assert!(matches!(
            next.await,
            Some(ChildStreamItem::ResyncRequired {
                reason: ChildResyncReason::HostReplaced,
                ..
            })
        ));
    }
    assert!(s.next().await.is_none());
    f.host.shutdown().await;
    f.close().await;
}

#[tokio::test]
async fn missing_incoming_fact_sparse_or_exhausted_scan_is_not_proof() {
    for mode in ["incoming", "sparse", "empty", "exact"] {
        let f = Fixture::plain().await;
        let mut s = f.subscribe().await;
        let incoming = event(&phase(2, "final"));
        // Bypass the oracle wrapper to model a stale event or native query omission.
        f.publish.inner.send_projected(incoming).await;
        {
            let mut oracle = f.publish.oracle.lock().unwrap();
            if mode == "sparse" {
                oracle.sparse = true;
            }
            if mode == "empty" {
                oracle.phases.clear();
            }
            if mode == "exact" {
                let mut p = phase(2, "final");
                p.is_error = Some(true); // same entry/index/seq is not exact equality
                oracle.phases.get_mut(CHILD).unwrap().insert(0, p);
            }
        }
        let before = f.publish.read_count();
        reason(&mut s, ChildResyncReason::BranchChanged).await;
        assert_eq!(f.publish.read_count() - before, 2); // NEVER follow continuation
        f.close().await;
    }
}

#[tokio::test]
async fn native_proof_failure_and_retired_bare_host_are_sanitized() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    f.publish.oracle.lock().unwrap().fail_next = true;
    f.publish.send_projected(event(&phase(2, "final"))).await;
    reason(&mut s, ChildResyncReason::BranchUnavailable).await;
    f.close().await;
    let f = BareFixture::plain().await;
    let mut s = child_subscription_for_host(f.host.clone(), request())
        .await
        .unwrap();
    f.publish.send_projected(event(&phase(2, "final"))).await;
    f.host.block_resource_storage();
    let before = f.publish.read_count();
    reason(&mut s, ChildResyncReason::BranchUnavailable).await;
    assert_eq!(f.publish.read_count(), before);
    f.close().await;
}

#[tokio::test]
async fn idle_invalid_and_outside_window_events_do_not_issue_proof_reads() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    let before = f.publish.read_count();
    {
        let next = s.next();
        tokio::pin!(next);
        assert!(futures_util::poll!(&mut next).is_pending());
    }
    assert_eq!(f.publish.read_count(), before);
    f.publish
        .send_projected(json!({"type":"session_entry_committed"}))
        .await;
    f.publish.send_projected(begin(OTHER, 20)).await;
    reason(&mut s, ChildResyncReason::OutsideWindow).await;
    assert_eq!(f.publish.read_count(), before);
    f.close().await;
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    let before = f.publish.read_count();
    let mut invalid = event(&phase(2, "final"));
    invalid["lane"] = json!("foreign");
    f.publish.send_projected(invalid).await;
    reason(&mut s, ChildResyncReason::InvalidChildEvent).await;
    assert_eq!(f.publish.read_count(), before);
    f.close().await;
}

#[tokio::test]
async fn eight_selected_children_require_at_most_ten_bounded_read_rpcs() {
    let f = Fixture::plain().await;
    {
        let mut oracle = f.publish.oracle.lock().unwrap();
        for n in 2..9 {
            let id = format!("tpchild-{n:08x}-2222-4222-8222-222222222222");
            oracle.others.push(summary(&id, 11 - n));
        }
    }
    let mut s = f.subscribe().await;
    assert_eq!(s.working.len(), 8);
    let before = f.publish.read_count();
    f.publish.send_projected(event(&phase(2, "final"))).await;
    update(&mut s, ReplayDisposition::Applied).await;
    assert_eq!(f.publish.read_count() - before, 10);
    {
        let oracle = f.publish.oracle.lock().unwrap();
        let reads = &oracle.reads[before..];
        assert_eq!(reads.first().unwrap()["type"], "get_tool_program_children");
        assert_eq!(reads.last().unwrap()["type"], "get_tool_program_children");
        for child_id in s.working.keys() {
            assert_eq!(
                reads.iter().filter(|q| q["childId"] == *child_id).count(),
                1
            );
        }
        assert!(reads
            .iter()
            .all(|q| q["maxBytes"] == 32768 && q["cursor"].is_null()));
    }
    f.close().await;
}

#[tokio::test(start_paused = true)]
async fn whole_branch_proof_has_one_thirty_second_budget_not_per_read_budgets() {
    let f = Fixture::plain().await;
    let mut s = f.subscribe().await;
    hold_proof_read(&f, 1);
    f.publish.send_projected(event(&phase(2, "final"))).await;
    {
        let next = s.next();
        tokio::pin!(next);
        tokio::select! {
            _ = f.phase_seen.notified() => {},
            item = &mut next => panic!("delivered before proof: {item:?}"),
        }
        tokio::time::advance(Duration::from_secs(29)).await;
        hold_proof_read(&f, 1); // hold the phase read after releasing the head
        f.release.notify_one();
        tokio::select! {
            _ = f.phase_seen.notified() => {},
            item = &mut next => panic!("delivered before proof: {item:?}"),
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(
            next.await,
            Some(ChildStreamItem::ResyncRequired {
                reason: ChildResyncReason::BranchUnavailable,
                ..
            })
        ));
    }
    assert!(s.next().await.is_none());
    assert_eq!(s.cursor, s.snapshot.last_seq);
    f.release.notify_one();
    f.close().await;
}

#[tokio::test]
async fn facts_of_nonincoming_selected_child_must_also_remain_on_branch() {
    let f = Fixture::new(1024, vec![], false, false, true, ready()).await;
    let mut other_fact = phase(0, "requested");
    other_fact.child_id = OTHER.into();
    other_fact.entry_id = "tpphase-44444444-4444-4444-8444-444444444444".into();
    other_fact.seq = 15;
    other_fact.descriptor.as_mut().unwrap().child_id = OTHER.into();
    {
        let mut oracle = f.publish.oracle.lock().unwrap();
        oracle.phases.insert(OTHER.into(), vec![other_fact.clone()]);
        oracle
            .snapshot_phases
            .insert(OTHER.into(), vec![other_fact]);
    }
    let mut s = f.subscribe().await;
    assert_eq!(s.working[OTHER].len(), 1);
    f.publish.oracle.lock().unwrap().phases.remove(OTHER);
    let mut incoming = phase(2, "final");
    incoming.seq = 20;
    f.publish.send_projected(event(&incoming)).await;
    reason(&mut s, ChildResyncReason::BranchChanged).await;
    assert_eq!(
        snapshot::observe(CHILD, &s.working[CHILD]).outcome,
        ChildOutcome::Unknown
    );
    f.close().await;
}
