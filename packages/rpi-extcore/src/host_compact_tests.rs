#[tokio::test]
async fn compact_uses_prompt_lease_subscription_and_cancellation() {
    for instructions in [None, Some("keep decisions\n  and TODOs 🌊".to_owned())] {
        let (stdin, peer) = tokio::io::duplex(8192);
        let (session, injector) = HostSession::test_connection_with_ready(
            stdin,
            16,
            json!({"supportedCommands":["compact"]}),
        );
        let mut lines = BufReader::new(peer).lines();
        let mut run = session
            .begin_compaction(instructions.clone())
            .await
            .unwrap();
        let request: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["type"], "compact");
        assert!(request.get("message").is_none());
        assert_eq!(
            request.get("customInstructions").and_then(Value::as_str),
            instructions.as_deref()
        );
        assert!(session.begin_prompt("no".into()).await.is_err());
        assert!(session.begin_compaction(None).await.is_err());
        injector.send(json!({"type":"response","id":request["id"],"success":true,"data":{"accepted":true}})).unwrap();
        assert!(run
            .tracker
            .observe(&run.events.recv().await.unwrap(), false)
            .outcome
            .is_none());
        session.abort_run().await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap()
                ["type"],
            "abort"
        );
        assert!(run.cancellation.has_changed().unwrap());
        run.cancellation.changed().await.unwrap();
        assert!(session.try_run_lease().is_err(), "abort is not settlement");
        injector
            .send(json!({"type":"agent_settled","outcome":"aborted"}))
            .unwrap();
        assert_eq!(
            run.tracker
                .observe(&run.events.recv().await.unwrap(), true)
                .outcome,
            Some(Ok(crate::run::RunOutcome::Cancelled))
        );
        assert!(
            session.try_run_lease().is_err(),
            "run owns lease until dropped"
        );
        drop(run);
        let prompt = session.begin_prompt("ordinary".into()).await.unwrap();
        assert!(session.begin_compaction(None).await.is_err());
        drop(prompt);
        session.terminate().await;
        assert!(session.begin_compaction(None).await.is_err());
        assert!(
            session.try_run_lease().is_ok(),
            "send failure releases lease"
        );
    }
}

#[tokio::test]
async fn compact_requires_explicit_native_capability_without_sending() {
    for commands in [Value::Null, json!([]), json!(["prompt"]), json!("compact")] {
        let (stdin, peer) = tokio::io::duplex(8192);
        let (session, _injector) = HostSession::test_connection_with_ready(
            stdin,
            16,
            json!({"supportedCommands":commands}),
        );
        assert!(session
            .begin_compaction(None)
            .await
            .err()
            .unwrap()
            .contains("does not support compact"));
        assert!(session.try_run_lease().is_ok());
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            BufReader::new(peer).lines().next_line()
        )
        .await
        .is_err());
        session.terminate().await;
    }
}
