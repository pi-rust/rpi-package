use super::*;

// Legacy race tests share a fixture under QUEUE_TEST_LOCK. Production has
// no static registry; independent instance tests use McpRuntime::new().
fn test_runtime() -> &'static McpRuntime {
    static FIXTURE: std::sync::OnceLock<McpRuntime> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(McpRuntime::new)
}
fn test_scope() -> &'static Mutex<Option<String>> {
    static SCOPE: Mutex<Option<String>> = Mutex::new(None);
    &SCOPE
}
struct TestScopeGuard(Option<String>);
impl TestScopeGuard {
    fn new() -> Self {
        Self(test_scope().lock().unwrap().clone())
    }
}
impl Drop for TestScopeGuard {
    fn drop(&mut self) {
        *test_scope().lock().unwrap() = self.0.clone();
        clear_queue();
    }
}
fn flush_resource_updates(
    append: impl FnOnce(Vec<ResourceUpdate>) -> Result<(), String>,
) -> Result<(), String> {
    test_runtime().deliver_resource_updates(|| test_scope().lock().unwrap().clone(), append)
}

#[test]
fn resource_text_flattens_text_and_notes_blobs() {
    let text = resource_text(&json!({"contents": [
            {"uri": "res://a", "mimeType": "text/plain", "text": "hello"},
            {"uri": "res://b", "blob": "QUJD"}]}));
    assert!(text.contains("res://a"));
    assert!(text.contains("hello"));
    assert!(text.contains("[blob, 4 base64 chars]"), "got: {text}");
}

#[test]
fn resource_text_bounds_regular_and_unexpected_responses() {
    let huge = "中".repeat(MAX_RESOURCE_TEXT);
    for response in [
        json!({"contents":[{"uri":"mem://large","text":huge}]}),
        json!({"unexpected":huge}),
    ] {
        let text = resource_text(&response);
        assert!(
            text.len() <= MAX_RESOURCE_TEXT + "\n[truncated]".len(),
            "unbounded resource response: {} bytes",
            text.len()
        );
        assert!(text.ends_with("\n[truncated]"));
    }
}

/// The queue is process-global, so these tests must not interleave.
static QUEUE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn queue_test_guard() -> MutexGuard<'static, ()> {
    QUEUE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn queue(scope: &str, key: &str, uri: &str, text: &str) {
    let token = test_runtime().subscription(key, uri).unwrap_or_default();
    test_runtime()
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(QueuedResourceUpdate::new(
            ResourceUpdate {
                scope: scope.into(),
                key: key.into(),
                uri: uri.into(),
                text: text.into(),
            },
            token,
        ));
}

fn clear_queue() {
    test_runtime()
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
}

#[test]
fn a_new_subscription_lifetime_drops_the_previous_queued_content() {
    let _serial = queue_test_guard();
    clear_queue();
    const KEY: &str = r#"["session:renew","config","server"]"#;
    const URI: &str = "mem://renew";
    test_runtime().add_subscription(KEY, URI);
    queue("session:renew", KEY, URI, "obsolete content");
    test_runtime().add_subscription(KEY, URI);
    let obsolete = test_runtime().take_pending_updates(None);
    queue("session:renew", KEY, URI, "fresh content");
    let fresh = test_runtime().take_pending_updates(None);
    test_runtime().clear_subscriptions(KEY);
    assert!(
        obsolete.is_empty(),
        "old subscription content survived renewal: {obsolete:?}"
    );
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].text, "fresh content");
}

#[test]
fn failed_host_append_preserves_resource_updates_for_the_next_boundary() {
    let _serial = queue_test_guard();
    let _scope = TestScopeGuard::new();
    clear_queue();
    *test_scope().lock().unwrap() = Some("session:delivery-retry".into());
    const KEY: &str = r#"["session:delivery-retry","config","server"]"#;
    const URI: &str = "mem://delivery";
    test_runtime().add_subscription(KEY, URI);
    let notification = json!({"method":"notifications/resources/updated","params":{"uri":URI}});
    test_runtime()
        .dispatch_notification_with(KEY, &notification, |_, _| Ok("current content".into()));
    assert!(flush_resource_updates(|_| Err("host unavailable".into())).is_err());
    assert_eq!(
        test_runtime().pending.lock().unwrap().len(),
        1,
        "failed append lost the update"
    );
    let mut delivered = None;
    flush_resource_updates(|message| {
        delivered = Some(message);
        Ok(())
    })
    .unwrap();
    test_runtime().clear_subscriptions(KEY);
    assert!(delivered.unwrap()[0].text.contains("current content"));
    flush_resource_updates(|_| panic!("successfully delivered update was replayed")).unwrap();
}

#[test]
fn failed_delivery_cannot_revive_retired_or_superseded_resource_content() {
    let _serial = queue_test_guard();
    let _scope = TestScopeGuard::new();
    *test_scope().lock().unwrap() = Some("session:delivery-lifetime".into());
    const KEY: &str = r#"["session:delivery-lifetime","config","server"]"#;
    const URI: &str = "mem://lifetime";
    for action in [
        "unsubscribe",
        "close",
        "replace",
        "newer-read",
        "newer-delivered",
    ] {
        clear_queue();
        test_runtime().add_subscription(KEY, URI);
        queue("session:delivery-lifetime", KEY, URI, "obsolete content");
        let mut delivered = Vec::new();
        let result = flush_resource_updates(|_| {
            match action {
                "unsubscribe" => test_runtime().remove_subscription(KEY, URI, None),
                "close" => test_runtime().clear_subscriptions(KEY),
                _ => {
                    if action == "replace" {
                        test_runtime().add_subscription(KEY, URI);
                    }
                    queue("session:delivery-lifetime", KEY, URI, "fresh content");
                    if action == "newer-delivered" {
                        // Another boundary already delivered the newer read
                        // before this older append reports failure.
                        flush_resource_updates(|message| {
                            delivered.push(message);
                            Ok(())
                        })
                        .unwrap();
                    }
                }
            }
            Err("host unavailable".into())
        });
        assert!(result.is_err());
        flush_resource_updates(|message| {
            delivered.push(message);
            Ok(())
        })
        .unwrap();
        test_runtime().clear_subscriptions(KEY);
        if matches!(action, "unsubscribe" | "close") {
            assert!(delivered.is_empty(), "{action}: {delivered:?}");
        } else {
            assert_eq!(delivered.len(), 1, "{action}: {delivered:?}");
            let text = delivered[0][0].text.as_str();
            assert!(text.contains("fresh content"), "{action}: {text}");
            assert!(!text.contains("obsolete content"), "{action}: {text}");
        }
    }
}

#[test]
fn failed_delivery_drops_updates_after_a_session_switch() {
    let _serial = queue_test_guard();
    let _scope = TestScopeGuard::new();
    clear_queue();
    const KEY_A: &str = r#"["session:delivery-a","config","server"]"#;
    const KEY_B: &str = r#"["session:delivery-b","config","server"]"#;
    const URI: &str = "mem://session";
    *test_scope().lock().unwrap() = Some("session:delivery-a".into());
    test_runtime().add_subscription(KEY_A, URI);
    queue("session:delivery-a", KEY_A, URI, "A content");
    assert!(flush_resource_updates(|_| {
        *test_scope().lock().unwrap() = Some("session:delivery-b".into());
        test_runtime().add_subscription(KEY_B, URI);
        queue("session:delivery-b", KEY_B, URI, "B content");
        Err("host switched session".into())
    })
    .is_err());
    let updates = test_runtime().take_pending_updates(None);
    test_runtime().clear_subscriptions(KEY_A);
    test_runtime().clear_subscriptions(KEY_B);
    assert_eq!(updates.len(), 1, "old session was requeued: {updates:?}");
    assert_eq!(updates[0].text, "B content");
}

#[test]
fn failed_delivery_keeps_the_queue_bounded_and_prefers_new_updates() {
    let _serial = queue_test_guard();
    let _scope = TestScopeGuard::new();
    clear_queue();
    *test_scope().lock().unwrap() = Some("session:delivery-bounds".into());
    const KEY: &str = r#"["session:delivery-bounds","config","server"]"#;
    test_runtime().add_subscription(KEY, "mem://old");
    queue("session:delivery-bounds", KEY, "mem://old", "old content");
    assert!(flush_resource_updates(|_| {
        for index in 0..MAX_PENDING {
            let uri = format!("mem://fresh-{index}");
            test_runtime().add_subscription(KEY, &uri);
            queue("session:delivery-bounds", KEY, &uri, "fresh content");
        }
        Err("host unavailable".into())
    })
    .is_err());
    let updates = test_runtime().take_pending_updates(None);
    test_runtime().clear_subscriptions(KEY);
    assert_eq!(updates.len(), MAX_PENDING);
    assert!(updates.iter().all(|update| update.text == "fresh content"));
}

fn assert_teardown_retires_late_subscription(shutdown: bool) {
    let _serial = queue_test_guard();
    clear_queue();
    const KEY: &str = r#"["session:closing","config","server"]"#;
    const URI: &str = "mem://closing";
    let (notifications, _receiver) = mpsc::sync_channel(1);
    let entry = Arc::new(Entry {
        session: Mutex::new(ConnectionState::default()),
        stop: Control::default(),
        notifications,
        dispatcher: Mutex::new(None),
    });
    test_runtime()
        .sessions
        .lock()
        .unwrap()
        .insert(KEY.into(), entry.clone());
    // Subscribe mutates its local lifetime under this connection lock,
    // before its RPC observes cancellation. Pause it immediately before
    // that mutation, then start teardown on another thread.
    let state = entry.session.lock().unwrap();
    let closing = std::thread::spawn(move || {
        if shutdown {
            test_runtime().shutdown_connections();
        } else {
            test_runtime().close_key(KEY).unwrap();
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while entry.stop.check().is_ok() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let cancelled = entry.stop.check().is_err();
    test_runtime().add_subscription(KEY, URI);
    queue("session:closing", KEY, URI, "late content");
    drop(state);
    closing.join().unwrap();
    let retained = test_runtime().subscription(KEY, URI).is_some();
    let updates = test_runtime().take_pending_updates(None);
    test_runtime().clear_subscriptions(KEY);
    assert!(cancelled, "teardown did not cancel the active operation");
    assert!(
        !retained,
        "teardown left an in-flight subscription registered"
    );
    assert!(
        updates.is_empty(),
        "teardown left queued content: {updates:?}"
    );
    assert!(!test_runtime().sessions.lock().unwrap().contains_key(KEY));
}

#[test]
fn close_retires_subscriptions_after_in_flight_operations_exit() {
    assert_teardown_retires_late_subscription(false);
}

#[test]
fn closing_connection_stays_registered_until_its_dispatcher_exits() {
    let _serial = queue_test_guard();
    const KEY: &str = r#"["session:dispatcher-close","config","server"]"#;
    let stop = Control::default();
    let worker_stop = stop.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let dispatcher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while worker_stop.check().is_ok() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        ready_tx.send(()).unwrap();
        // Model a notification already being processed when close begins.
        let _ = resume_rx.recv_timeout(Duration::from_secs(3));
    });
    let (notifications, _receiver) = mpsc::sync_channel(1);
    let entry = Arc::new(Entry {
        session: Mutex::new(ConnectionState::default()),
        stop,
        notifications,
        dispatcher: Mutex::new(Some(dispatcher)),
    });
    test_runtime()
        .sessions
        .lock()
        .unwrap()
        .insert(KEY.into(), entry.clone());
    let closing = std::thread::spawn(|| test_runtime().close_key(KEY));
    let ready = ready_rx.recv_timeout(Duration::from_secs(3));
    // Removing the key early lets a new call replace it while the old
    // dispatcher can still resolve it in read_resource_by_key.
    let retained = test_runtime()
        .sessions
        .lock()
        .unwrap()
        .get(KEY)
        .is_some_and(|current| Arc::ptr_eq(current, &entry));
    let _ = resume_tx.send(());
    closing.join().unwrap().unwrap();
    assert!(ready.is_ok(), "dispatcher fixture did not observe close");
    assert!(
        retained,
        "a replacement could open before the old dispatcher exited"
    );
    assert!(!test_runtime().sessions.lock().unwrap().contains_key(KEY));
}

#[test]
fn shutdown_retires_subscriptions_after_in_flight_operations_exit() {
    // Shutdown intentionally affects every registry entry. Isolate this
    // test from parallel HTTP lifecycle tests that also use the registry.
    const CHILD: &str = "RPI_MCP_SHUTDOWN_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        assert_teardown_retires_late_subscription(true);
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime::tests::shutdown_retires_subscriptions_after_in_flight_operations_exit",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn repeated_retirement_cannot_clear_a_replacement_connections_subscriptions() {
    let _serial = queue_test_guard();
    clear_queue();
    const KEY: &str = r#"["session:replacement","config","server"]"#;
    const URI: &str = "mem://replacement";
    let make_entry = || {
        let (notifications, _receiver) = mpsc::sync_channel(1);
        Arc::new(Entry {
            session: Mutex::new(ConnectionState::default()),
            stop: Control::default(),
            notifications,
            dispatcher: Mutex::new(None),
        })
    };
    let old = make_entry();
    test_runtime()
        .sessions
        .lock()
        .unwrap()
        .insert(KEY.into(), old.clone());
    test_runtime().close_key(KEY).unwrap();
    let current = make_entry();
    test_runtime()
        .sessions
        .lock()
        .unwrap()
        .insert(KEY.into(), current.clone());
    let token = test_runtime().add_subscription(KEY, URI);
    queue("session:replacement", KEY, URI, "new content");
    // A concurrent shutdown may have captured the old entry before close
    // finished, then reach its cleanup after the key has been reopened.
    test_runtime().release_entry(KEY, &old).unwrap();
    let retained = test_runtime()
        .subscription(KEY, URI)
        .is_some_and(|current| Arc::ptr_eq(&current, &token));
    let still_registered = test_runtime()
        .sessions
        .lock()
        .unwrap()
        .get(KEY)
        .is_some_and(|entry| Arc::ptr_eq(entry, &current));
    let updates = test_runtime().take_pending_updates(None);
    test_runtime().close_key(KEY).unwrap();
    assert!(retained, "old cleanup cleared the replacement subscription");
    assert!(
        still_registered,
        "old cleanup removed the replacement connection"
    );
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].text, "new content");
}

#[test]
fn in_flight_read_cannot_enqueue_after_subscription_retirement() {
    let _serial = queue_test_guard();
    const KEY: &str = r#"["session:race","config","server"]"#;
    const URI: &str = "mem://race";
    let notification = json!({"method":"notifications/resources/updated","params":{"uri":URI}});
    for action in ["unsubscribe", "close-and-reopen", "replace"] {
        clear_queue();
        test_runtime().add_subscription(KEY, URI);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let value = notification.clone();
        let worker = std::thread::spawn(move || {
            test_runtime().dispatch_notification_with(KEY, &value, |_, _| {
                // Pause a successful read just before the dispatcher can
                // enqueue it, reproducing the original retirement race.
                ready_tx.send(()).unwrap();
                resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok("obsolete content".into())
            });
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        match action {
            "unsubscribe" => test_runtime().remove_subscription(KEY, URI, None),
            "close-and-reopen" => {
                test_runtime().clear_subscriptions(KEY);
                test_runtime().add_subscription(KEY, URI);
            }
            "replace" => {
                test_runtime().add_subscription(KEY, URI);
            }
            _ => unreachable!(),
        }
        queue("session:other", "other-key", URI, "unrelated");
        resume_tx.send(()).unwrap();
        worker.join().unwrap();
        let updates = test_runtime().take_pending_updates(None);
        assert_eq!(
            updates.len(),
            1,
            "{action}: stale read was queued: {updates:?}"
        );
        assert_eq!(updates[0].text, "unrelated");

        if action != "unsubscribe" {
            test_runtime()
                .dispatch_notification_with(KEY, &notification, |_, _| Ok("fresh content".into()));
            let updates = test_runtime().take_pending_updates(None);
            assert_eq!(
                updates.len(),
                1,
                "{action}: new subscription stopped working"
            );
            assert_eq!(updates[0].text, "fresh content");
        }
        test_runtime().clear_subscriptions(KEY);
    }
}

#[test]
fn stale_failure_cannot_remove_a_new_subscription_or_its_updates() {
    let _serial = queue_test_guard();
    clear_queue();
    let old = test_runtime().add_subscription("key", "mem://a");
    let current = test_runtime().add_subscription("key", "mem://a");
    queue("session:a", "key", "mem://a", "fresh");
    test_runtime().remove_subscription("key", "mem://a", Some(&old));
    assert!(Arc::ptr_eq(
        &test_runtime().subscription("key", "mem://a").unwrap(),
        &current
    ));
    assert_eq!(test_runtime().take_pending_updates(None).len(), 1);
    test_runtime().remove_subscription("key", "mem://a", Some(&current));
    assert!(test_runtime().subscription("key", "mem://a").is_none());
}

/// Regression: the queue was global and `TurnEnd` drained all of it, so
/// session A's resource contents could land in session B's context.
#[test]
fn queued_updates_are_scoped_to_the_session_that_produced_them() {
    let _serial = queue_test_guard();
    clear_queue();
    queue("session:a", "key-a", "mem://a", "A");
    queue("session:b", "key-b", "mem://b", "B");

    // The host is serving B: only B's update may reach the model.
    let served = test_runtime().take_pending_updates(Some("session:b"));
    assert_eq!(served.len(), 1, "got {served:?}");
    assert_eq!(served[0].text, "B");
    // A's update was dropped, not deferred into the next drain.
    assert!(test_runtime()
        .take_pending_updates(Some("session:a"))
        .is_empty());
    clear_queue();
}

/// A host with no session id (older, or an ephemeral session) still gets
/// everything rather than nothing.
#[test]
fn unscoped_host_still_receives_updates() {
    let _serial = queue_test_guard();
    clear_queue();
    queue("library", "key-l", "mem://l", "L");
    let served = test_runtime().take_pending_updates(None);
    assert_eq!(served.len(), 1);
    clear_queue();
}

/// Closing a connection (or unsubscribing one URI) drops exactly its queued
/// updates, and leaves other connections' alone.
#[test]
fn close_and_unsubscribe_drop_only_their_own_updates() {
    let _serial = queue_test_guard();
    clear_queue();
    queue("session:a", "key-a", "mem://a", "A");
    queue("session:b", "key-b", "mem://b", "B");

    // Unsubscribing one URI drops exactly that update.
    test_runtime().clear_pending_for_uri("key-a", "mem://a");
    let remaining = test_runtime().take_pending_updates(None);
    assert_eq!(remaining.len(), 1, "got {remaining:?}");
    assert_eq!(remaining[0].text, "B");

    // Closing one connection drops all of its updates, not the other's.
    queue("session:a", "key-a", "mem://other", "A2");
    queue("session:b", "key-b", "mem://b", "B2");
    test_runtime().clear_pending_for_key("key-a");
    let remaining = test_runtime().take_pending_updates(None);
    assert_eq!(remaining.len(), 1, "got {remaining:?}");
    assert_eq!(remaining[0].text, "B2");
    clear_queue();
}

/// The scope stored with an update is the one embedded in its connection key
/// (`[scope, identity, name]`), which is what the filter above compares.
#[test]
fn scope_is_read_back_out_of_the_connection_key() {
    let key = serde_json::to_string(&("session:abc", "/tmp/mcp.json", "fake")).unwrap();
    assert_eq!(scope_of_key(&key), "session:abc");
    assert_eq!(scope_of_key("not json"), "");
}

#[test]
fn dropping_last_runtime_owner_joins_dispatcher_and_releases_state() {
    let owner = Arc::new(McpRuntime::new());
    let last_owner = owner.clone();
    let weak = Arc::downgrade(&owner.state);
    let key = r#"["same-scope","same-config","same-server"]"#.to_string();
    let (notifications, receiver) = mpsc::sync_channel(1);
    let stop = Control::default();
    let dispatcher = owner.spawn_dispatcher(key.clone(), receiver, stop.clone());
    let entry = Arc::new(Entry {
        session: Mutex::new(ConnectionState::default()),
        stop,
        notifications,
        dispatcher: Mutex::new(Some(dispatcher)),
    });
    owner.sessions.lock().unwrap().insert(key, entry.clone());
    drop(owner);
    assert!(entry.stop.check().is_ok());
    assert!(entry.dispatcher.lock().unwrap().is_some());
    drop(last_owner);
    assert!(entry.stop.check().is_err());
    assert!(
        entry.dispatcher.lock().unwrap().is_none(),
        "dispatcher was not joined"
    );
    assert!(
        weak.upgrade().is_none(),
        "dispatcher retained runtime state"
    );
}
