#[test]
fn list_parser_accepts_optional_nulls_and_fixes_wire_limit() {
    for params in [
        Value::Null,
        json!({}),
        json!({"cwd":null,"cursor":null}),
        json!({"limit":9999,"_meta":{}}),
    ] {
        let query = parse(&params).unwrap();
        assert_eq!(query.limit, 50);
        assert!(query.cwd.is_none());
        assert!(query.cursor.is_none());
    }
    let cwd = std::env::temp_dir();
    let query = parse(&json!({"cwd":cwd,"cursor":"opaque-token"})).unwrap();
    assert_eq!(query.cwd, Some(cwd));
    assert_eq!(query.cursor.as_deref(), Some("opaque-token"));
}

#[test]
fn list_parser_rejects_wrong_types_relative_paths_and_unsafe_cursors() {
    for params in [
        json!(false),
        json!(1),
        json!([]),
        json!("x"),
        json!({"cwd":false}),
        json!({"cwd":[]}),
        json!({"cwd":"relative"}),
        json!({"cwd":""}),
        json!({"cwd":"/nul\u{0}path"}),
        json!({"cursor":1}),
        json!({"cursor":{}}),
        json!({"cursor":""}),
        json!({"cursor":"a\nb"}),
        json!({"cursor":"白"}),
        json!({"cursor":" ","cwd":null}),
        json!({"cursor":"x".repeat(257)}),
    ] {
        assert!(parse(&params).is_err(), "{params}");
    }
    assert!(parse(&json!({"cursor":"x".repeat(256)})).is_ok());
    #[cfg(windows)]
    for cwd in ["C:relative", "\\rooted", "/rooted"] {
        assert!(parse(&json!({"cwd":cwd})).is_err());
    }
}

#[test]
fn utc_dates_are_safe_at_epoch_leaps_and_four_digit_boundaries() {
    for (ms, date) in [
        (-62_167_219_200_000, "0000-01-01T00:00:00.000Z"),
        (-62_135_596_800_000, "0001-01-01T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (0, "1970-01-01T00:00:00.000Z"),
        (951_782_400_000, "2000-02-29T00:00:00.000Z"),
        (1_709_251_199_123, "2024-02-29T23:59:59.123Z"),
        (4_107_542_400_000, "2100-03-01T00:00:00.000Z"),
        (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
    ] {
        assert_eq!(utc_date(ms).as_deref(), Some(date), "{ms}");
    }
    for ms in [i64::MIN, i64::MAX, -62_167_219_200_001, 253_402_300_800_000] {
        assert!(utc_date(ms).is_none());
    }
}

#[test]
fn response_omits_absent_or_unrepresentable_dates_titles_and_cursor() {
    use rpi_extcore::session_catalog::SessionSummary;
    let cwd = std::env::temp_dir();
    let page = CatalogPage {
        sessions: [None, Some(i64::MIN), Some(0)]
            .into_iter()
            .enumerate()
            .map(|(i, modified_at_ms)| SessionSummary {
                id: i.to_string(),
                cwd: cwd.clone(),
                created_at_ms: 123,
                modified_at_ms,
            })
            .collect(),
        next_cursor: None,
    };
    let result = format_page(page).unwrap();
    assert!(result.get("nextCursor").is_none());
    assert_eq!(result["sessions"][0], json!({"sessionId":"0","cwd":cwd}));
    assert_eq!(result["sessions"][1], json!({"sessionId":"1","cwd":cwd}));
    assert_eq!(
        result["sessions"][2],
        json!({"sessionId":"2","cwd":cwd,"updatedAt":"1970-01-01T00:00:00.000Z"})
    );
    assert_eq!(
        format_page(CatalogPage {
            sessions: vec![],
            next_cursor: Some("opaque".into())
        })
        .unwrap(),
        json!({"sessions":[],"nextCursor":"opaque"})
    );
}

#[test]
fn all_catalog_errors_have_fixed_sanitized_wire_categories() {
    for (error, code) in [
        (CatalogError::InvalidInput, -32602),
        (CatalogError::InvalidCursor, -32602),
        (CatalogError::Cancelled, -32800),
        (CatalogError::DeadlineExceeded, -32800),
        (CatalogError::LimitExceeded, -32001),
        (CatalogError::AccessDenied, -32001),
        (CatalogError::Conflict, -32001),
        (CatalogError::Unavailable, -32001),
    ] {
        let (actual, message) = wire_error(&error);
        assert_eq!(actual, code);
        assert!(message.starts_with("session/list") || message.starts_with("invalid session/list"));
        assert!(!message.contains(':'));
    }
}

#[tokio::test]
async fn task_bound_cancel_and_shutdown_keep_ownership_without_native() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let root = std::env::temp_dir().join(format!("acp-list-missing-{}", uuid::Uuid::new_v4()));
    let mut tasks = ListTasks::new(Ok(root.clone()));
    assert!(!root.exists());
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    let mut lines = BufReader::new(peer).lines();
    // A current-thread test cannot poll either child before these calls return.
    tasks
        .start(json!(1), "1".into(), &Value::Null, writer.clone())
        .await;
    tasks
        .start(json!("1"), "\"1\"".into(), &Value::Null, writer.clone())
        .await;
    tasks
        .start(json!(3), "3".into(), &Value::Null, writer.clone())
        .await;
    let rejection: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(rejection["id"], 3);
    assert_eq!(rejection["error"]["code"], -32001);
    assert!(tasks.cancel("1"));
    assert!(!tasks.cancel("missing"));
    tasks.shutdown().await;
    let mut ids = Vec::new();
    for _ in 0..2 {
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        ids.push(response["id"].clone());
        assert_eq!(response["error"]["code"], -32800);
        assert!(response.get("result").is_none());
    }
    assert!(ids.contains(&json!(1)) && ids.contains(&json!("1")));
    assert!(tasks.tasks.is_empty());
    assert!(!root.exists());
}

#[tokio::test]
async fn dropping_list_tasks_cancels_controls_and_signals_closing() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let root = std::env::temp_dir().join(format!("acp-list-drop-{}", uuid::Uuid::new_v4()));
    let mut tasks = ListTasks::new(Ok(root.clone()));
    let mut closing = tasks.closing.subscribe();
    let (output, peer) = tokio::io::duplex(8192);
    let writer = Arc::new(Writer::with_output(output));
    tasks
        .start(json!(1), "1".into(), &Value::Null, writer)
        .await;
    let control = tasks.tasks["1"].control.clone();
    drop(tasks);
    assert_eq!(control.check(), Err(CatalogError::Cancelled));
    closing.changed().await.unwrap();
    assert!(*closing.borrow());
    // Drop does not abort/detach the core page; the owned task settles and may
    // still deliver its immediately writable single cancellation completion.
    let mut lines = BufReader::new(peer).lines();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["id"], 1);
        assert_eq!(response["error"]["code"], -32800);
        assert!(lines.next_line().await.unwrap().is_none());
    })
    .await
    .unwrap();
    assert!(!root.exists());
}
