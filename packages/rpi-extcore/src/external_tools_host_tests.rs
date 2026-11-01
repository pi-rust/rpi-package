// Included in host::tests so these fixtures exercise the real correlation and writer.
fn external_fixture() -> (Arc<HostSession>, TestEvents, tokio::io::DuplexStream) {
    let (writer, reader) = tokio::io::duplex(65536);
    let (session, events) = HostSession::test_connection_with_ready(
        writer,
        2,
        json!({
            "type":"ready", "protocol":"rpi",
            "supportedCommands":["configure_external_tools","external_tool_response"],
            "supportedEvents":["external_tool_request","external_tool_request_closed"]
        }),
    );
    (session, events, reader)
}
fn empty_tool_configuration() -> crate::external_tools::Configuration {
    crate::external_tools::Configuration {
        servers: vec![],
        cwd: std::env::current_dir().unwrap(),
        scope: "fixture".into(),
    }
}
async fn empty_candidate() -> (
    crate::external_tools::Candidate,
    crate::external_tools::Catalog,
) {
    let mut candidate =
        crate::external_tools::Candidate::start(empty_tool_configuration()).unwrap();
    let catalog = candidate.discover().await.unwrap();
    (candidate, catalog)
}
async fn install_empty_bridge(
    session: &HostSession,
    events: &TestEvents,
    reader: &mut BufReader<tokio::io::DuplexStream>,
) {
    let (candidate, catalog) = empty_candidate().await;
    let install =
        session.install_tool_catalog(candidate, catalog, session.cancellation.subscribe());
    let peer = async {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let command: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(command["type"], "configure_external_tools");
        assert_eq!(command["tools"], json!([]));
        events
            .send(json!({"type":"response","id":command["id"],"success":true,"data":{"tools":[]}}))
            .unwrap();
    };
    let (result, _) = tokio::join!(install, peer);
    result.unwrap();
}

#[tokio::test]
async fn external_empty_old_host_and_explicit_live_identity() {
    let (writer, _peer) = tokio::io::duplex(4096);
    let (session, _events) = HostSession::test_connection(writer, 2);
    let config = empty_tool_configuration();
    session
        .configure_stdio_tools(vec![], config.cwd.clone(), config.scope.clone())
        .await
        .unwrap();
    session
        .configure_stdio_tools(vec![], config.cwd.clone(), config.scope)
        .await
        .unwrap();
    assert!(session
        .configure_stdio_tools(vec![], config.cwd, "changed".into())
        .await
        .unwrap_err()
        .contains("close and reopen"));
    assert!(session.external_owner.lock().unwrap().is_none());
}

#[tokio::test]
async fn external_unknown_native_rejected_before_spawn_and_configuration_holds_run_lease() {
    let (writer, _peer) = tokio::io::duplex(4096);
    let (session, _events) = HostSession::test_connection(writer, 2);
    let server = crate::external_tools::StdioToolServer {
        name: "fixture".into(),
        command: "must-not-spawn".into(),
        args: vec![],
        env: Default::default(),
    };
    assert_eq!(
        session
            .configure_stdio_tools(
                vec![server.clone()],
                std::env::current_dir().unwrap(),
                "fixture".into()
            )
            .await
            .unwrap_err(),
        "native host does not support external tools"
    );
    let _lease = session.try_run_lease().unwrap();
    assert!(session
        .configure_stdio_tools(
            vec![server],
            std::env::current_dir().unwrap(),
            "fixture".into(),
        )
        .await
        .unwrap_err()
        .contains("already running"));
}

#[tokio::test]
async fn external_rejected_atomic_batch_never_installs_candidate_or_echoes_error() {
    let (session, events, peer) = external_fixture();
    let mut peer = BufReader::new(peer);
    let (candidate, catalog) = empty_candidate().await;
    let install =
        session.install_tool_catalog(candidate, catalog, session.cancellation.subscribe());
    let reject = async {
        let mut line = String::new();
        peer.read_line(&mut line).await.unwrap();
        let command: Value = serde_json::from_str(&line).unwrap();
        events.send(json!({"type":"response","id":command["id"],"success":false,"error":"SECRET schema contents"})).unwrap();
    };
    let (result, _) = tokio::join!(install, reject);
    assert_eq!(result.unwrap_err(), "native tool configuration rejected");
    assert!(session.external_owner.lock().unwrap().is_none());
    assert!(session.external_config.lock().await.is_none());
    assert!(!*session.runtime.closed_signal.borrow());
}

#[tokio::test]
async fn external_registration_acknowledgement_preserves_order_and_fails_closed_on_ambiguity() {
    for shape in [
        "ordered",
        "reordered",
        "missing",
        "duplicate",
        "extra",
        "absent",
    ] {
        let (session, events, peer) = external_fixture();
        let mut peer = BufReader::new(peer);
        let (candidate, mut catalog) = empty_candidate().await;
        // Isolate acknowledgement/correlation from MCP discovery. Native accepts
        // in input order, not lexicographic registry order. Bindings are unused.
        let names = [
            format!("mcp_{}", "b".repeat(60)),
            format!("mcp_{}", "a".repeat(60)),
        ];
        catalog.definitions = names.iter().map(|name| json!({"name":name,"description":"ack fixture","inputSchema":{"type":"object"}})).collect();
        let install =
            session.install_tool_catalog(candidate, catalog, session.cancellation.subscribe());
        let acknowledge = async {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), peer.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["tools"][0]["name"], names[0]);
            assert_eq!(request["tools"][1]["name"], names[1]);
            let mut data = json!({"tools":names});
            match shape {
                "reordered" => data["tools"] = json!([names[1], names[0]]),
                "missing" => data["tools"] = json!([names[0]]),
                "duplicate" => data["tools"] = json!([names[0], names[0]]),
                "extra" => data["tools"] = json!([names[0], names[1], "extra"]),
                "absent" => data = json!({}),
                _ => {}
            }
            events
                .send(json!({"type":"response","id":request["id"],"success":true,"data":data}))
                .unwrap();
        };
        let (result, _) = tokio::join!(install, acknowledge);
        if shape == "ordered" {
            result.unwrap();
            assert!(session.external_owner.lock().unwrap().is_some());
            assert!(!*session.runtime.closed_signal.borrow());
        } else {
            assert_eq!(
                result.unwrap_err(),
                "native tool configuration acknowledgement is invalid"
            );
            assert!(session.external_owner.lock().unwrap().is_none());
            assert!(*session.runtime.closed_signal.borrow());
        }
        session.terminate().await;
    }
}

#[tokio::test]
async fn external_cancelled_native_registration_fails_closed_and_releases_lease() {
    let (session, _events, peer) = external_fixture();
    let mut peer = BufReader::new(peer);
    let (candidate, catalog) = empty_candidate().await;
    let caller_session = session.clone();
    let caller = tokio::spawn(async move {
        let _lease = caller_session.try_run_lease().unwrap();
        caller_session
            .install_tool_catalog(candidate, catalog, caller_session.cancellation.subscribe())
            .await
    });
    let mut line = String::new();
    peer.read_line(&mut line).await.unwrap();
    caller.abort();
    let _ = caller.await;
    assert!(*session.runtime.closed_signal.borrow());
    assert!(session.external_owner.lock().unwrap().is_none());
    assert!(session.try_run_lease().is_ok());
}

#[tokio::test]
async fn external_reliable_reader_routing_and_closed_request_suppression() {
    let (session, events, peer) = external_fixture();
    let mut peer = BufReader::new(peer);
    install_empty_bridge(&session, &events, &mut peer).await;
    // No broadcast subscribers exist. A same-tick close must suppress the call.
    events.send(json!({"type":"external_tool_request","requestId":"closed","toolName":"unknown","arguments":{}})).ok();
    events
        .send(json!({"type":"external_tool_request_closed","requestId":"closed"}))
        .ok();
    events.send(json!({"type":"external_tool_request","requestId":"live","toolName":"unknown","arguments":{}})).ok();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(2), peer.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["requestId"], "live");
    assert_eq!(response["error"], "unknown external tool");
    events
        .send(
            json!({"type":"response","id":response["id"],"success":true,"data":{"accepted":true}}),
        )
        .ok();
    events
        .send(json!({"type":"external_tool_request_closed","requestId":"live"}))
        .ok();
    session.terminate().await;
}

#[tokio::test]
async fn external_abort_preserves_owner_and_overflow_fails_closed() {
    let (session, events, peer) = external_fixture();
    let mut peer = BufReader::new(peer);
    install_empty_bridge(&session, &events, &mut peer).await;
    let owner = session.runtime.bridge().unwrap();
    events.send(json!({"type":"external_tool_request","requestId":"cancel","toolName":"unknown","arguments":{}})).ok();
    session.abort_run().await.unwrap();
    assert!(Arc::ptr_eq(&owner, &session.runtime.bridge().unwrap()));
    // Synchronous reader admission is bounded even before supervisor scheduling.
    for id in 0..65 {
        events.send(json!({"type":"external_tool_request","requestId":id.to_string(),"toolName":"unknown","arguments":{}})).ok();
    }
    assert!(*session.runtime.closed_signal.borrow());
    owner.join().await;
}

#[tokio::test]
async fn external_owner_drop_has_no_session_or_bridge_cycle() {
    let (session, events, peer) = external_fixture();
    let mut peer = BufReader::new(peer);
    install_empty_bridge(&session, &events, &mut peer).await;
    let weak_session = Arc::downgrade(&session);
    let weak_owner = Arc::downgrade(&session.runtime.bridge().unwrap());
    drop(events);
    drop(session);
    assert!(weak_session.upgrade().is_none());
    assert!(weak_owner.upgrade().is_none());
    tokio::task::yield_now().await;
}

#[tokio::test]
async fn external_cancelled_queued_response_is_not_written() {
    let (commands, outbound) = mpsc::channel(2);
    let pending = Arc::new(StdMutex::new(HashMap::new()));
    let control = rpi_mcp_core::Control::default();
    commands
        .send(Outbound {
            value: json!({"type":"external_tool_response","requestId":"retired","error":"no"}),
            pending_id: None,
            control: Some(CommandControl::Tools(control.clone())),
        })
        .await
        .unwrap();
    control.cancel();
    drop(commands);
    let mut bytes = Vec::new();
    write_commands(&mut bytes, outbound, pending).await.unwrap();
    assert!(bytes.is_empty());
}
