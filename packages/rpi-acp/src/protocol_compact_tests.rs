#[tokio::test]
async fn compact_announcement_follows_session_response_and_is_capability_gated() {
    for available in [false, true] {
        let (output, peer) = tokio::io::duplex(8192);
        let writer = Writer::with_output(output);
        let mut lines = BufReader::new(peer).lines();
        writer
            .respond_session(&json!(42), json!({"sessionId":"s"}), "s", available)
            .await;
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], 42);
        assert_eq!(response["result"]["sessionId"], "s");
        if available {
            let update: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert!(update.get("id").is_none());
            assert_eq!(update["method"], "session/update");
            assert_eq!(update["params"]["sessionId"], "s");
            assert_eq!(
                update["params"]["update"]["sessionUpdate"],
                "available_commands_update"
            );
            let commands = update["params"]["update"]["availableCommands"]
                .as_array()
                .unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0]["name"], "compact");
            assert!(commands[0]["input"]["hint"].is_string());
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line())
                .await
                .is_err()
        );
    }
}
