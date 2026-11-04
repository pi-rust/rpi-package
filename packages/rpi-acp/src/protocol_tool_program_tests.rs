//! In-memory native RPC peers exercised through the production ACP dispatcher.
use super::*;
use rpi_extcore::host::HostSession;
use rpi_plugin_sdk::stdio::InputLine;
use std::{sync::Mutex as StdMutex, time::Duration};
use tokio::io::{DuplexStream, Lines};
use tokio::sync::mpsc;

const SESSION: &str = "session";
const CHILD: &str = "tpchild-11111111-1111-4111-8111-111111111111";
const PHASE: &str = "tpphase-22222222-2222-4222-8222-222222222222";
const BODY: &str = "33333333-3333-4333-8333-333333333333";

fn ready() -> Value {
    json!({"sessionId":SESSION,"toolProgramChildren":{
        "version":1,"summaryCommand":"get_tool_program_children",
        "phaseCommand":"get_tool_program_child_phases","bodyCommand":"get_tool_program_child_body",
        "maxCount":32,"maxPhaseCandidates":128,"maxResponseBytes":131072,"responseBytesScope":"pageBytes",
        "maxBodyDecodedBytes":65536,"summaryState":"unknown","recovery":"readOnly"},
        "toolProgramChildEvents":rpi_extcore::service::tool_program_events::profile(),
        "supportedEvents":["tool_program_child_begin","tool_program_child_phase","tool_program_child_resync_required"],
        "supportedCommands":["get_tool_program_children","get_tool_program_child_phases","get_tool_program_child_body"]})
}
fn page_query() -> Value {
    json!({"leafId":null,"cursor":null,"direction":"newestFirst","limit":8,"maxBytes":32768})
}
fn queries() -> Vec<Value> {
    vec![
        page_query(),
        json!({"childId":CHILD,"direction":"newestFirst","limit":8,"maxBytes":32768}),
        json!({"childId":CHILD,"phaseEntryId":PHASE,"descriptorId":format!("tpbody-{BODY}-commit"),"limit":1,"maxBytes":65536}),
        json!({"children":page_query(),"phaseLimit":8}),
    ]
}
fn summary_page() -> Value {
    json!({"leafId":"leaf","children":[{"version":1,"childId":CHILD,"seq":10,
        "origin":{"runId":"run","assistantEntryId":"assistant","toolIndex":0},
        "originVerification":"forkCopy","rootCallId":"call","rootToolName":"program",
        "toolName":"read","ordinal":0,"parentChildId":null,"state":"unknown"}],
        "nextCursor":null,"scanExhausted":true})
}
fn phase_page() -> Value {
    json!({"leafId":"leaf","childId":CHILD,"phases":[{"entryId":PHASE,"seq":20,"version":1,
        "childId":CHILD,"index":4,"phase":"closed","descriptor":null,"isError":null,"cancelled":null,
        "failed":false,"interrupted":true}],"nextCursor":null,"scanned":1,"scanExhausted":true,"state":"unknown"})
}
fn body_page() -> Value {
    // SHA-256 of {}, canonical base64 e30=; no new hashing dependency needed.
    let hash = "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
    json!({"childId":CHILD,"phaseEntryId":PHASE,"body":{"descriptor":{
        "descriptorId":format!("tpbody-{BODY}-commit"),"version":1,"bodyId":BODY,"childId":CHILD,
        "kind":"final","codec":"native-json-v1","encoding":"base64","byteLength":2,
        "chunkBytes":32768,"chunks":1,"sha256":hash},"leafId":"leaf","byteOffset":0,
        "byteLength":2,"totalBytes":2,"chunks":[{"index":0,"byteOffset":0,"byteLength":2,
        "data":"e30=","sha256":hash}],"nextCursor":null},"nextCursor":null})
}

struct Fixture {
    session: Arc<Session>,
    requests: Arc<StdMutex<Vec<Value>>>,
    reject: Arc<std::sync::atomic::AtomicBool>,
    events: rpi_extcore::host::TestEvents,
    worker: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(ready: Value) -> Self {
        let (writer, peer) = tokio::io::duplex(65536);
        let (host, events) = HostSession::test_connection_with_ready(writer, 64, ready);
        let response_events = events.clone();
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let recorded = requests.clone();
        let reject = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rejected = reject.clone();
        let worker = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let response = if rejected.load(Ordering::Acquire) {
                    json!({"type":"response","id":request["id"],"success":false,
                        "error":"SECRET C:/private/native-debug.txt\ninternal error"})
                } else {
                    let data = match request["type"].as_str().unwrap() {
                        "get_tool_program_children" => {
                            let mut page = summary_page();
                            if request["limit"] == 1 {
                                page["scanExhausted"] = json!(false);
                                page["nextCursor"] = json!({"sessionId":SESSION,"lane":"main",
                                    "leafId":"leaf","direction":"newestFirst","childId":null,
                                    "afterEntryId":CHILD,"afterSeq":10});
                            }
                            page
                        }
                        "get_tool_program_child_phases" => phase_page(),
                        "get_tool_program_child_body" => body_page(),
                        command => panic!("unexpected native command: {command}"),
                    };
                    json!({"type":"response","id":request["id"],"success":true,"data":data})
                };
                if response_events.send(response).is_err() {
                    break;
                }
            }
        });
        Self {
            session: Session::attach_for_test(host).await.unwrap(),
            requests,
            reject,
            events,
            worker,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

struct Client {
    input: mpsc::UnboundedSender<String>,
    output: Lines<BufReader<DuplexStream>>,
}
impl Client {
    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.input
            .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string())
            .unwrap();
        let line = tokio::time::timeout(Duration::from_secs(3), self.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], id, "no unsolicited child notifications");
        value
    }
}

async fn connection<F, Fut>(fixture: &Fixture, scenario: F)
where
    F: FnOnce(Client) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let sessions = Arc::new(Mutex::new(HashMap::from([(
        SESSION.into(),
        fixture.session.clone(),
    )])));
    let (output, peer) = tokio::io::duplex(65536);
    let writer = Arc::new(Writer::with_output(output));
    let (input, mut requests) = mpsc::unbounded_channel();
    let client = Client {
        input,
        output: BufReader::new(peer).lines(),
    };
    let (stop_tx, stop) = watch::channel(false);
    let server = run_connection_with_sessions(
        stop,
        || {
            Ok(match requests.try_recv() {
                Ok(line) => InputLine::Line(line),
                Err(mpsc::error::TryRecvError::Empty) => InputLine::Pending,
                Err(mpsc::error::TryRecvError::Disconnected) => InputLine::Eof,
            })
        },
        None,
        writer,
        Err("unused list root".into()),
        sessions,
    );
    let scenario = async {
        scenario(client).await;
        stop_tx.send_replace(true);
    };
    let (exit, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(server, scenario)
    })
    .await
    .unwrap();
    assert_eq!(exit, 0);
}

#[tokio::test]
async fn dispatcher_projects_all_four_typed_reads_without_client_capability() {
    let fixture = Fixture::new(ready()).await;
    connection(&fixture, |mut client| async move {
        for (index, query) in queries().into_iter().enumerate() {
            let frame = client
                .request(
                    index as u64,
                    tool_program_queries::METHODS[index],
                    json!({"sessionId":SESSION,"query":query,"_meta":{}}),
                )
                .await;
            assert!(frame.get("error").is_none(), "{frame}");
            let result = &frame["result"];
            match index {
                0 => assert_eq!(*result, summary_page()),
                1 => assert_eq!(*result, phase_page()),
                2 => assert_eq!(*result, body_page()),
                3 => {
                    assert_eq!(result["sessionId"], SESSION);
                    assert_eq!(result["lane"], "main");
                    assert_eq!(result["page"], summary_page());
                    assert_eq!(result["phases"], json!([phase_page()]));
                    assert_eq!(result["observations"][0]["outcome"], "unknown");
                    assert_eq!(result["startSeq"], result["lastSeq"]);
                    assert_eq!(result["events"], json!([]));
                    assert_eq!(result["resyncRequired"], false);
                }
                _ => unreachable!(),
            }
        }
    })
    .await;
    let requests = fixture.requests.lock().unwrap();
    let commands: Vec<_> = requests
        .iter()
        .map(|v| v["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        commands,
        [
            "get_tool_program_children",
            "get_tool_program_child_phases",
            "get_tool_program_child_body",
            "get_tool_program_children",
            "get_tool_program_child_phases",
            "get_tool_program_children"
        ]
    );
    for request in requests.iter() {
        assert!(request.get("sessionId").is_none());
        assert!(request.get("_meta").is_none());
        assert!(request.get("query").is_none());
    }
}

#[tokio::test]
async fn live_child_subscription_is_snapshot_first_and_unsubscribes_cleanly() {
    let fixture = Fixture::new(ready()).await;
    let events = fixture.events.clone();
    connection(&fixture, |mut client| async move {
        let response = client
            .request(
                40,
                tool_program_live::SUBSCRIBE_METHOD,
                json!({
                    "sessionId": SESSION,
                    "subscriptionId": "sub-1",
                    "query": queries()[3],
                }),
            )
            .await;
        assert_eq!(response["result"]["sessionId"], SESSION);
        assert_eq!(response["result"]["subscriptionId"], "sub-1");
        assert_eq!(response["result"]["snapshot"]["events"], json!([]));

        let duplicate = client
            .request(
                42,
                tool_program_live::SUBSCRIBE_METHOD,
                json!({
                    "sessionId": SESSION,
                    "subscriptionId": "sub-1",
                    "query": queries()[3],
                }),
            )
            .await;
        assert_eq!(duplicate["error"]["code"], -32602);
        assert_eq!(duplicate["error"]["message"], "duplicate subscriptionId");

        events
            .send_projected(json!({
                "type":"tool_program_child_begin", "schemaVersion":1,
                "sourceSessionId":SESSION, "lane":"main", "entryId":CHILD,
                "parentEntryId":null, "seq":10,
                "customType":"rpi.tool_program.child_begin.v1",
                "data":{"version":1, "childId":CHILD,
                    "origin":{"runId":"run","assistantEntryId":"assistant","toolIndex":0},
                    "rootCallId":"call", "rootToolName":"program", "toolName":"read",
                    "ordinal":0, "parentChildId":null}
            }))
            .await;
        let line = tokio::time::timeout(Duration::from_secs(3), client.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let notification: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            notification["method"],
            tool_program_live::NOTIFICATION_METHOD
        );
        assert_eq!(notification["params"]["subscriptionId"], "sub-1");
        assert!(notification["params"]["update"].is_object());

        let unsubscribed = client
            .request(
                41,
                tool_program_live::UNSUBSCRIBE_METHOD,
                json!({"sessionId":SESSION,"subscriptionId":"sub-1"}),
            )
            .await;
        assert_eq!(unsubscribed["result"], json!({}));
    })
    .await;
}

#[tokio::test]
async fn strict_params_cursors_and_bounds_reject_before_native_write() {
    let fixture = Fixture::new(ready()).await;
    let mut cases = vec![];
    for (index, query) in queries().into_iter().enumerate() {
        let valid = json!({"sessionId":SESSION,"query":query});
        for id in [
            Value::Null,
            json!(3),
            json!(""),
            json!(" "),
            json!("bad\n"),
            json!("x".repeat(513)),
        ] {
            let mut bad = valid.clone();
            bad["sessionId"] = id;
            cases.push((index, bad));
        }
        for meta in [Value::Null, json!([]), json!(false)] {
            let mut bad = valid.clone();
            bad["_meta"] = meta;
            cases.push((index, bad));
        }
        let mut bad = valid.clone();
        bad["execute"] = json!(true);
        cases.push((index, bad));
        let mut bad = valid.clone();
        bad["query"]["unknown"] = json!(true);
        cases.push((index, bad));
        let mut bad = valid.clone();
        bad["query"] = json!("not typed");
        cases.push((index, bad));
        let mut bad = valid.clone();
        bad.as_object_mut().unwrap().remove("query");
        cases.push((index, bad));
    }
    for value in [json!(0), json!(33), json!(-1), json!("8"), json!(1.5)] {
        let mut q = page_query();
        q["limit"] = value;
        cases.push((0, json!({"sessionId":SESSION,"query":q})));
    }
    for value in [json!(0), json!(131073)] {
        let mut q = page_query();
        q["maxBytes"] = value;
        cases.push((0, json!({"sessionId":SESSION,"query":q})));
    }
    for cursor in [
        json!({}),
        json!({"sessionId":"foreign","lane":"main","leafId":"leaf","direction":"newestFirst","childId":null,"afterEntryId":CHILD,"afterSeq":10}),
        json!({"sessionId":SESSION,"lane":"main","leafId":"leaf","direction":"newestFirst","childId":null,"afterEntryId":CHILD,"afterSeq":10,"extra":true}),
    ] {
        let mut q = page_query();
        q["cursor"] = cursor;
        cases.push((0, json!({"sessionId":SESSION,"query":q})));
    }
    for (index, key, value) in [
        (1, "childId", json!("bad")),
        (2, "limit", json!(9)),
        (2, "phaseEntryId", json!("bad")),
        (3, "phaseLimit", json!(9)),
    ] {
        let mut q = queries()[index].clone();
        q[key] = value;
        cases.push((index, json!({"sessionId":SESSION,"query":q})));
    }
    let mut q = queries()[3].clone();
    q["children"]["extra"] = json!(1);
    cases.push((3, json!({"sessionId":SESSION,"query":q})));
    connection(&fixture, |mut client| async move {
        for (id, (method, params)) in cases.into_iter().enumerate() {
            let frame = client
                .request(id as u64, tool_program_queries::METHODS[method], params)
                .await;
            assert_eq!(frame["error"]["code"], -32602, "{frame}");
        }
    })
    .await;
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn old_host_is_explicitly_unsupported_without_requests_or_fallback() {
    let fixture = Fixture::new(json!({"sessionId":SESSION,"supportedCommands":[]})).await;
    connection(&fixture, |mut client| async move {
        for (id, q) in queries().into_iter().enumerate() {
            let frame = client
                .request(
                    id as u64,
                    tool_program_queries::METHODS[id],
                    json!({"sessionId":SESSION,"query":q}),
                )
                .await;
            assert_eq!(frame["error"]["code"], -32001);
            assert_eq!(
                frame["error"]["data"]["code"],
                if id == 3 {
                    "unsupported"
                } else {
                    "tool_program_queries_unsupported"
                }
            );
        }
    })
    .await;
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn native_rejection_is_sanitized_and_does_not_disclose_debug_output() {
    let fixture = Fixture::new(ready()).await;
    fixture.reject.store(true, Ordering::Release);
    connection(&fixture, |mut client| async move {
        let frame = client.request(1, tool_program_queries::METHODS[0], json!({"sessionId":SESSION,"query":page_query()})).await;
        assert_eq!(frame["error"], json!({"code":-32001,"message":"native child query rejected","data":{"code":"host_unavailable"}}));
        assert!(!frame.to_string().contains("SECRET"));
    }).await;
}

#[tokio::test]
async fn unknown_session_legacy_method_and_initialize_remain_compatible() {
    let fixture = Fixture::new(ready()).await;
    connection(&fixture, |mut client| async move {
        for (id, q) in queries().into_iter().enumerate() {
            let frame = client
                .request(
                    id as u64,
                    tool_program_queries::METHODS[id],
                    json!({"sessionId":"not-open","query":q}),
                )
                .await;
            assert_eq!(frame["error"]["code"], -32602);
            assert_eq!(frame["error"]["message"], "unknown sessionId");
        }
        let unknown = client
            .request(5, "session/tool_program_execute", json!({}))
            .await;
        assert_eq!(unknown["error"]["code"], -32601);
        let initialized = client
            .request(
                6,
                "initialize",
                json!({"protocolVersion":1,"clientCapabilities":{}}),
            )
            .await;
        let result = &initialized["result"];
        assert_eq!(result["protocolVersion"], 1);
        assert_eq!(result["authMethods"], json!([]));
        assert_eq!(result["agentCapabilities"]["loadSession"], true);
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"],
            json!({"image":true,"audio":false,"embeddedContext":true})
        );
        assert_eq!(
            result["agentCapabilities"]["_meta"]["rpi/durable"]["notifications"],
            json!(["session/durable_update"])
        );
        let cap = &result["agentCapabilities"]["_meta"]["rpi/tool-program-children"];
        assert_eq!(cap["version"], 1);
        assert_eq!(cap["optional"], true);
        assert_eq!(cap["recovery"], "readOnly");
        assert_eq!(cap["methods"], json!(tool_program_queries::METHODS));
        assert_eq!(cap["live"]["snapshotFirst"], true);
        assert_eq!(cap["live"]["maxSubscriptions"], 16);
        assert_eq!(
            cap["live"]["notification"],
            tool_program_live::NOTIFICATION_METHOD
        );
    })
    .await;
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dropped_query_and_caller_timeout_ignore_late_native_responses() {
    let (writer, peer) = tokio::io::duplex(8192);
    let (host, events) = HostSession::test_connection_with_ready(writer, 64, ready());
    let session = Session::attach_for_test(host).await.unwrap();
    let mut lines = BufReader::new(peer).lines();
    let query = || serde_json::from_value(page_query()).unwrap();
    let first_session = session.clone();
    let first = tokio::spawn(async move { first_session.tool_program_children(query()).await });
    let first_request: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    // A caller's timeout drops the owned core request future as well. This is
    // complemented below by ingress-level ACP cancellation tests.
    let second_session = session.clone();
    let second = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_millis(20),
            second_session.tool_program_children(query()),
        )
        .await
    });
    let second_request: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert!(second.await.unwrap().is_err());

    let third_session = session.clone();
    let third = tokio::spawn(async move { third_session.tool_program_children(query()).await });
    let third_request: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_ne!(first_request["id"], third_request["id"]);
    assert_ne!(second_request["id"], third_request["id"]);
    for request in [first_request, second_request] {
        // The fixture reports an unmatched frame when correlation was removed.
        assert!(events
            .send(json!({"type":"response","id":request["id"],"success":false,
            "error":"late SECRET native error"}))
            .is_err());
    }
    events
        .send(
            json!({"type":"response","id":third_request["id"],"success":true,
        "data":summary_page()}),
        )
        .unwrap();
    let page = tokio::time::timeout(Duration::from_secs(2), third)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(page).unwrap(), summary_page());
    session.kill().await;
}

/// A deliberately silent native peer; no worker can accidentally finish a read.
struct HeldConnection {
    client: Client,
    native: Lines<BufReader<DuplexStream>>,
    events: rpi_extcore::host::TestEvents,
    writer: Arc<Writer>,
    stop: watch::Sender<bool>,
    server: tokio::task::JoinHandle<i32>,
}
impl HeldConnection {
    async fn new() -> Self {
        let (output, peer) = tokio::io::duplex(65536);
        Self::with_writer(Arc::new(Writer::with_output(output)), peer).await
    }
    async fn with_writer(writer: Arc<Writer>, peer: DuplexStream) -> Self {
        let (native_writer, native_peer) = tokio::io::duplex(65536);
        let (host, events) = HostSession::test_connection_with_ready(native_writer, 64, ready());
        let session = Session::attach_for_test(host).await.unwrap();
        let sessions = Arc::new(Mutex::new(HashMap::from([(SESSION.into(), session)])));
        let (input, mut requests) = mpsc::unbounded_channel();
        let (stop_tx, stop) = watch::channel(false);
        let task_writer = writer.clone();
        let server = tokio::spawn(async move {
            run_connection_with_sessions(
                stop,
                || {
                    Ok(match requests.try_recv() {
                        Ok(line) => InputLine::Line(line),
                        Err(mpsc::error::TryRecvError::Empty) => InputLine::Pending,
                        Err(mpsc::error::TryRecvError::Disconnected) => InputLine::Eof,
                    })
                },
                None,
                task_writer,
                Err("unused root".into()),
                sessions,
            )
            .await
        });
        Self {
            client: Client {
                input,
                output: BufReader::new(peer).lines(),
            },
            native: BufReader::new(native_peer).lines(),
            events,
            writer,
            stop: stop_tx,
            server,
        }
    }
    fn send(&self, id: Value, method: &str, params: Value) {
        self.client
            .input
            .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string())
            .unwrap();
    }
    async fn query(&mut self, id: Value) -> Value {
        self.send(
            id,
            tool_program_queries::METHODS[0],
            json!({"sessionId":SESSION,"query":page_query()}),
        );
        self.native_request().await
    }
    async fn native_request(&mut self) -> Value {
        serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(2), self.native.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    async fn frame(&mut self) -> Value {
        serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(2), self.client.output.next_line())
                .await
                .expect("ACP dispatcher stalled")
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    fn late_is_retired(&self, request: &Value) {
        assert!(self.events.send(json!({"type":"response","id":request["id"],"success":true,"data":summary_page()})).is_err(), "native correlation must be retired");
    }
    async fn no_frame(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(40), self.client.output.next_line())
                .await
                .is_err(),
            "unexpected late query completion"
        );
    }
    async fn finish(&mut self, host_stop: bool) {
        if host_stop {
            self.stop.send_replace(true);
        } else {
            let (replacement, _) = mpsc::unbounded_channel();
            self.client.input = replacement; // real ingress EOF
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), &mut self.server)
                .await
                .expect("query teardown stalled")
                .unwrap(),
            0
        );
    }
}
impl Drop for HeldConnection {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.writer.close();
        self.server.abort(); // failure-only cleanup; normal finish joins
    }
}

#[tokio::test]
async fn ingress_pending_query_services_ordinary_request_and_exact_cancel_without_native_abort() {
    let mut c = HeldConnection::new().await;
    let request = c.query(json!(1)).await;
    // Simultaneously outstanding agent->client numeric ID must not be cancelled.
    let (tx, mut rx) = oneshot::channel();
    c.writer.pending.lock().unwrap().insert(
        1,
        ClientPending {
            tx,
            max_response_bytes: None,
        },
    );
    let ordinary = c
        .client
        .request(2, "initialize", json!({"protocolVersion":1}))
        .await;
    assert_eq!(ordinary["result"]["protocolVersion"], 1);
    assert_eq!(
        c.client
            .request(3, "$/cancel_request", json!({"requestId":1}))
            .await["result"],
        json!({})
    );
    assert_eq!(c.writer.pending_count(), 1);
    assert!(matches!(
        rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    c.late_is_retired(&request);
    c.no_frame().await;
    // No Native.abort; the shared host remains usable after query-only cancel.
    let next = c.query(json!(4)).await;
    assert_eq!(next["type"], "get_tool_program_children");
    c.events
        .send(json!({"type":"response","id":next["id"],"success":true,"data":summary_page()}))
        .unwrap();
    assert_eq!(c.frame().await["id"], 4);
    c.finish(false).await;
}

#[tokio::test]
async fn ingress_duplicate_ids_cover_prompt_list_query_and_preserve_numeric_vs_string() {
    let mut c = HeldConnection::new().await;
    let numeric = c.query(json!(1)).await;
    for method in [
        "session/prompt",
        "session/list",
        "session/tool_program_children",
        "initialize",
    ] {
        c.send(json!(1), method, json!({}));
        let frame = c.frame().await;
        assert_eq!(frame["id"], 1);
        assert_eq!(frame["error"]["code"], -32600);
    }
    let string = c.query(json!("1")).await;
    assert_ne!(numeric["id"], string["id"]);
    assert_eq!(
        c.client
            .request(2, "$/cancel_request", json!({"requestId":1}))
            .await["result"],
        json!({})
    );
    c.late_is_retired(&numeric);
    c.events
        .send(json!({"type":"response","id":string["id"],"success":true,"data":summary_page()}))
        .unwrap();
    let frame = c.frame().await;
    assert_eq!(frame["id"], "1");
    assert_eq!(frame["result"], summary_page());
    assert_eq!(
        c.client
            .request(1, "initialize", json!({"protocolVersion":1}))
            .await["result"]["protocolVersion"],
        1
    );
    c.send(json!("1"), "initialize", json!({"protocolVersion":1}));
    assert_eq!(c.frame().await["result"]["protocolVersion"], 1);
    c.no_frame().await;
    c.finish(false).await;
}

#[tokio::test]
async fn ingress_session_cancel_retires_all_query_correlations_before_ack() {
    let mut c = HeldConnection::new().await;
    let first = c.query(json!(1)).await;
    let second = c.query(json!(2)).await;
    assert_eq!(
        c.client
            .request(3, "session/cancel", json!({"sessionId":SESSION}))
            .await["result"],
        json!({})
    );
    c.late_is_retired(&first);
    c.late_is_retired(&second);
    c.no_frame().await;
    c.finish(false).await;
}

#[tokio::test]
async fn ingress_close_and_delete_stop_queries_before_session_retirement() {
    for method in ["session/close", "session/delete"] {
        let mut c = HeldConnection::new().await;
        let pending = c.query(json!(1)).await;
        // The fixture is not a saved-session UUID. Delete retains its existing
        // validation error AFTER stopping the live session; no disk is touched.
        let completion = c
            .client
            .request(2, method, json!({"sessionId":SESSION}))
            .await;
        if method == "session/close" {
            assert_eq!(completion["result"], json!({}));
        } else {
            assert_eq!(completion["error"]["code"], -32602);
        }
        c.late_is_retired(&pending);
        c.no_frame().await;
        assert_eq!(
            c.client
                .request(
                    3,
                    tool_program_queries::METHODS[0],
                    json!({"sessionId":SESSION,"query":page_query()})
                )
                .await["error"]["message"],
            "unknown sessionId"
        );
        c.finish(false).await;
    }
}

#[tokio::test]
async fn ingress_eof_and_host_stop_drain_held_queries_without_late_response() {
    for host_stop in [false, true] {
        let mut c = HeldConnection::new().await;
        let first = c.query(json!(1)).await;
        let second = c.query(json!("1")).await;
        c.finish(host_stop).await;
        c.late_is_retired(&first);
        c.late_is_retired(&second);
        // The test retains Writer, so its pipe is still open but must be silent.
        c.no_frame().await;
    }
}

#[tokio::test]
async fn ingress_query_capacity_is_64_and_cancel_releases_slots() {
    let mut c = HeldConnection::new().await;
    let mut pending = Vec::new();
    for id in 0..64 {
        pending.push(c.query(json!(id)).await);
    }
    let full = c
        .client
        .request(
            64,
            tool_program_queries::METHODS[0],
            json!({"sessionId":SESSION,"query":page_query()}),
        )
        .await;
    assert_eq!(
        full["error"],
        json!({"code":-32001,"message":"child query task limit exceeded"})
    );
    assert_eq!(
        c.client
            .request(65, "session/cancel", json!({"sessionId":SESSION}))
            .await["result"],
        json!({})
    );
    for request in pending {
        c.late_is_retired(&request);
    }
    // Preserve the pre-existing session/cancel native abort behavior.
    assert_eq!(c.native_request().await["type"], "abort");
    let request = c.query(json!(64)).await;
    assert_eq!(request["type"], "get_tool_program_children");
    c.finish(false).await;
    c.late_is_retired(&request);
}

#[tokio::test]
async fn ingress_native_30_second_deadline_retires_query_without_replay() {
    let mut c = HeldConnection::new().await;
    let request = c.query(json!(1)).await;
    let line = tokio::time::timeout(Duration::from_secs(35), c.client.output.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(frame["id"], 1);
    assert_eq!(frame["error"]["code"], -32001);
    assert_eq!(frame["error"]["data"]["code"], "host_unavailable");
    c.late_is_retired(&request);
    c.no_frame().await;
    c.finish(false).await;
}

#[tokio::test]
async fn ingress_cancelling_partially_written_query_response_is_connection_terminal() {
    use super::protocol_configuration_tests::ObservedPending;
    use tokio::io::AsyncReadExt;
    let (output, peer) = tokio::io::duplex(8);
    let pending = Arc::new(tokio::sync::Notify::new());
    let writer = Arc::new(Writer::with_output(ObservedPending {
        output,
        pending: pending.clone(),
    }));
    let mut c = HeldConnection::with_writer(writer.clone(), peer).await;
    let request = c.query(json!(1)).await;
    c.events
        .send(json!({"type":"response","id":request["id"],"success":true,"data":summary_page()}))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), pending.notified())
        .await
        .unwrap();
    c.send(json!(2), "$/cancel_request", json!({"requestId":1}));
    tokio::time::timeout(Duration::from_secs(2), writer.terminal())
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), &mut c.server)
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(writer.closed.load(Ordering::Acquire));
    let mut bytes = [0; 32];
    let count = c.client.output.get_mut().read(&mut bytes).await.unwrap();
    assert_eq!(count, 8);
    assert!(!bytes[..count].contains(&b'\n')); // no promised cancellation ACK
}
