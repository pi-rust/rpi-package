mod retirement {
    use super::*;
    use tokio::io::{DuplexStream, Lines};

    type Peer = Lines<BufReader<DuplexStream>>;

    fn connection(history: bool) -> (Arc<HostSession>, TestEvents, Peer) {
        let (stdin, peer) = tokio::io::duplex(8192);
        let commands = if history {
            json!(["get_state", "get_history"])
        } else {
            json!(["get_state"])
        };
        let (host, events) = HostSession::test_connection_with_ready(
            stdin,
            32,
            json!({"supportedCommands":commands}),
        );
        (host, events, BufReader::new(peer).lines())
    }

    // Model the native reader staying alive until the fixture acknowledges
    // stop. test_connection otherwise has no reader worker, so shutdown is
    // allowed to abort its writer before the queued best-effort stop is read.
    async fn stop_ack_worker(host: &HostSession) -> oneshot::Sender<()> {
        let (ack, receive) = oneshot::channel();
        *host.worker.lock().await = Some(tokio::spawn(async move {
            let _ = receive.await;
        }));
        ack
    }

    fn idle() -> Value {
        json!({"activeOperation":null,"queue":{"steering":[],"followUp":[]}})
    }

    async fn frame(peer: &mut Peer, kind: &str) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(2), peer.next_line())
            .await
            .expect("command deadline")
            .unwrap()
            .expect("command frame");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], kind);
        value
    }

    fn reply(events: &TestEvents, request: &Value, success: bool, data: Value) {
        events
            .send(json!({"type":"response","id":request["id"],"success":success,"data":data}))
            .unwrap();
    }

    async fn no_command(peer: &mut Peer) {
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.next_line())
                .await
                .is_err(),
            "busy/unknown session must not receive stop or another command"
        );
    }

    #[tokio::test]
    async fn host_retirement_idle_reads_state_and_history_then_stops_and_closes() {
        let (host, events, mut peer) = connection(true);
        let ack = stop_ack_worker(&host).await;
        let closing = tokio::spawn({
            let host = host.clone();
            async move { host.shutdown_if_idle().await }
        });
        let request = frame(&mut peer, "get_state").await;
        reply(&events, &request, true, idle());
        let request = frame(&mut peer, "get_history").await;
        reply(&events, &request, true, json!({"openOperations":[]}));
        frame(&mut peer, "stop").await;
        ack.send(()).unwrap();
        assert_eq!(closing.await.unwrap(), Ok(()));
        assert_eq!(host.snapshot().await.status, "closed");
        assert!(*host.closed_receiver().borrow());
        assert!(host.send(json!({"type":"get_state"})).await.is_err());
        assert!(host.notify(json!({"type":"prompt"})).await.is_err());
    }

    #[tokio::test]
    async fn host_retirement_closed_is_idempotent_even_with_run_lease() {
        let (host, _events, _peer) = connection(false);
        let _run = host.try_run_lease().unwrap();
        host.terminate().await;
        assert_eq!(host.shutdown_if_idle().await, Ok(()));
        assert_eq!(host.shutdown_if_idle().await, Ok(()));
    }

    #[tokio::test]
    async fn host_retirement_run_lease_is_busy_without_stop() {
        let (host, _events, mut peer) = connection(false);
        let _run = host.try_run_lease().unwrap();
        assert_eq!(host.shutdown_if_idle().await, Err(IdleCloseError::Busy));
        no_command(&mut peer).await;
        assert!(!*host.closed_receiver().borrow());
    }

    #[tokio::test]
    async fn host_retirement_native_active_and_queues_are_busy_without_stop() {
        for data in [
            json!({"activeOperation":{"id":"active"},"queue":{"steering":[],"followUp":[]}}),
            json!({"activeOperation":null,"queue":{"steering":["queued"],"followUp":[]}}),
            json!({"activeOperation":null,"queue":{"steering":[],"followUp":["queued"]}}),
        ] {
            let (host, events, mut peer) = connection(false);
            let closing = tokio::spawn({
                let host = host.clone();
                async move { host.shutdown_if_idle().await }
            });
            let request = frame(&mut peer, "get_state").await;
            reply(&events, &request, true, data);
            assert_eq!(closing.await.unwrap(), Err(IdleCloseError::Busy));
            no_command(&mut peer).await;
            assert!(!*host.closed_receiver().borrow());
        }
    }

    #[tokio::test]
    async fn host_retirement_dialog_or_running_projection_is_busy_without_stop() {
        for status in ["ready", "running", "awaiting_input"] {
            let (host, _events, mut peer) = connection(false);
            {
                let mut state = host.runtime.state.write().await;
                state.status = status.into();
                if status == "ready" {
                    state.pending_dialogs.push(json!({"requestId":"dialog"}));
                }
            }
            assert_eq!(host.shutdown_if_idle().await, Err(IdleCloseError::Busy));
            no_command(&mut peer).await;
        }
    }

    #[tokio::test]
    async fn host_retirement_unknown_state_fails_closed_without_stop() {
        for (success, data) in [
            (true, json!({"queue":{"steering":[],"followUp":[]}})),
            (false, idle()),
            (true, Value::Null),
            (true, json!({"activeOperation":null})),
            (
                true,
                json!({"activeOperation":null,"queue":{"steeringCount":0,"followUpCount":0}}),
            ),
            (
                true,
                json!({"activeOperation":null,"queue":{"steering":[],"followUp":null}}),
            ),
        ] {
            let (host, events, mut peer) = connection(false);
            let closing = tokio::spawn({
                let host = host.clone();
                async move { host.shutdown_if_idle().await }
            });
            let request = frame(&mut peer, "get_state").await;
            reply(&events, &request, success, data);
            assert!(matches!(
                closing.await.unwrap(),
                Err(IdleCloseError::Unavailable(_))
            ));
            no_command(&mut peer).await;
            assert!(!*host.closed_receiver().borrow());
        }
    }

    #[tokio::test]
    async fn host_retirement_history_open_or_unknown_prevents_stop() {
        for (data, expected) in [
            (
                json!({"openOperations":[{"id":"open"}]}),
                IdleCloseError::Busy,
            ),
            (
                json!({}),
                IdleCloseError::Unavailable("get_history omitted openOperations array".into()),
            ),
        ] {
            let (host, events, mut peer) = connection(true);
            let closing = tokio::spawn({
                let host = host.clone();
                async move { host.shutdown_if_idle().await }
            });
            let request = frame(&mut peer, "get_state").await;
            reply(&events, &request, true, idle());
            let request = frame(&mut peer, "get_history").await;
            reply(&events, &request, true, data);
            assert_eq!(closing.await.unwrap(), Err(expected));
            no_command(&mut peer).await;
        }
    }

    #[tokio::test]
    async fn host_retirement_pending_dispatch_holds_admission() {
        let (host, events, mut peer) = connection(false);
        let pending = tokio::spawn({
            let host = host.clone();
            async move { host.send(json!({"type":"get_state"})).await }
        });
        let request = frame(&mut peer, "get_state").await;
        assert_eq!(host.shutdown_if_idle().await, Err(IdleCloseError::Busy));
        no_command(&mut peer).await;
        reply(&events, &request, true, idle());
        pending.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn host_retirement_waiting_old_arcs_cannot_dispatch_after_close() {
        let (host, events, mut peer) = connection(false);
        let ack = stop_ack_worker(&host).await;
        let closing = tokio::spawn({
            let host = host.clone();
            async move { host.shutdown_if_idle().await }
        });
        let request = frame(&mut peer, "get_state").await;
        // The writer is owned until shutdown completes. Both ordinary entry
        // points are pending behind it, with Arcs obtained before retirement.
        let send = host.send(json!({"type":"get_state"}));
        let notify = host.notify(json!({"type":"prompt","message":"must not run"}));
        tokio::pin!(send, notify);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut send)
            .await
            .is_err());
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut notify)
            .await
            .is_err());
        reply(&events, &request, true, idle());
        frame(&mut peer, "stop").await;
        ack.send(()).unwrap();
        assert_eq!(closing.await.unwrap(), Ok(()));
        assert!(send.await.is_err());
        assert!(notify.await.is_err());
        // Shutdown has aborted the writer: no queued sender got through.
        assert!(peer.next_line().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn host_retirement_rechecks_dialogs_after_probe() {
        let (host, events, mut peer) = connection(false);
        let closing = tokio::spawn({
            let host = host.clone();
            async move { host.shutdown_if_idle().await }
        });
        let request = frame(&mut peer, "get_state").await;
        host.runtime
            .state
            .write()
            .await
            .pending_dialogs
            .push(json!({"requestId":"late"}));
        reply(&events, &request, true, idle());
        assert_eq!(closing.await.unwrap(), Err(IdleCloseError::Busy));
        no_command(&mut peer).await;
    }

    #[tokio::test]
    async fn host_retirement_notified_execution_remains_busy_before_acceptance() {
        let (host, _events, mut peer) = connection(false);
        host.notify(json!({"type":"prompt","message":"work"}))
            .await
            .unwrap();
        frame(&mut peer, "prompt").await;
        assert_eq!(host.shutdown_if_idle().await, Err(IdleCloseError::Busy));
        no_command(&mut peer).await;
    }

    #[tokio::test]
    async fn host_retirement_unknown_projection_is_unavailable_and_error_displays() {
        let (host, _events, mut peer) = connection(false);
        host.runtime.state.write().await.status = "unknown".into();
        let error = host.shutdown_if_idle().await.unwrap_err();
        assert!(matches!(error, IdleCloseError::Unavailable(_)));
        assert!(error.to_string().contains("unknown"));
        assert_eq!(IdleCloseError::Busy.to_string(), "session is busy");
        no_command(&mut peer).await;
    }
}
