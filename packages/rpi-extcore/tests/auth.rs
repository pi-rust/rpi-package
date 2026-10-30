#![cfg(feature = "test-support")]
use rpi_extcore::auth::*;
use std::time::Duration;
fn options(provider: &str) -> AuthOptions {
    AuthOptions::new(env!("CARGO_BIN_EXE_rpi-extcore-auth-fixture"), provider)
}
fn response(id: &str, action: AuthAction) -> AuthResponse {
    AuthResponse {
        request_id: id.into(),
        action,
        content: None,
    }
}
async fn prompt(process: &mut AuthProcess) {
    assert!(matches!(
        process.next_event().await.unwrap(),
        Some(AuthEvent::Ready(_))
    ));
    assert!(matches!(
        process.next_event().await.unwrap(),
        Some(AuthEvent::Interaction(_))
    ));
}
#[tokio::test]
async fn describe_and_persisted_success_waits_for_exit() {
    let d = describe(&options("late_exit")).await.unwrap();
    assert!(d.rpc);
    assert_eq!(d.kind, AuthKind::Oauth);
    let mut p = AuthProcess::spawn(options("late_exit")).unwrap();
    prompt(&mut p).await;
    assert_eq!(
        p.respond(response("wrong", AuthAction::Accept)).await,
        Err(AuthError::Identity)
    );
    p.respond(response("one", AuthAction::Accept))
        .await
        .unwrap();
    assert_eq!(
        p.respond(response("one", AuthAction::Accept)).await,
        Err(AuthError::Identity)
    );
    let start = std::time::Instant::now();
    assert!(matches!(
        p.next_event().await.unwrap(),
        Some(AuthEvent::Result(AuthResult {
            authenticated: true,
            ..
        }))
    ));
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert!(p.next_event().await.unwrap().is_none());
    p.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancel_decline_and_secret_safe_failures() {
    for action in [AuthAction::Cancel, AuthAction::Decline] {
        let mut p = AuthProcess::spawn(options("normal")).unwrap();
        prompt(&mut p).await;
        p.respond(response("one", action)).await.unwrap();
        let event = p.next_event().await.unwrap().unwrap();
        assert!(matches!(
            event,
            AuthEvent::Result(AuthResult {
                authenticated: false,
                ..
            })
        ));
        assert!(!format!("{event:?}").contains("SECRET"));
        p.shutdown().await.unwrap();
    }
    for (provider, expected) in [
        ("wrong", AuthError::Identity),
        ("malformed", AuthError::Protocol),
        ("oversize", AuthError::Limit),
    ] {
        let mut p = AuthProcess::spawn(options(provider)).unwrap();
        let error = p.next_event().await.unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("SECRET"));
        p.shutdown().await.unwrap();
    }
    let mut p = AuthProcess::spawn(options("bad_exit")).unwrap();
    prompt(&mut p).await;
    p.respond(response("one", AuthAction::Accept))
        .await
        .unwrap();
    assert_eq!(p.next_event().await.unwrap_err(), AuthError::Exit);
}
#[tokio::test]
async fn prompt_timeout_and_shutdown() {
    let mut opts = options("normal");
    opts.prompt_timeout = Duration::from_millis(50);
    let mut p = AuthProcess::spawn(opts).unwrap();
    prompt(&mut p).await;
    assert_eq!(p.next_event().await.unwrap_err(), AuthError::Timeout);
    p.shutdown().await.unwrap();
    let mut p = AuthProcess::spawn(options("hang")).unwrap();
    tokio::time::timeout(Duration::from_secs(3), p.shutdown())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn drop_and_cancelled_caller_kill_child() {
    for cancel_caller in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        let mut opts = options("hang");
        opts.env
            .insert("AUTH_FIXTURE_PID".into(), path.as_os_str().into());
        let mut p = AuthProcess::spawn(opts).unwrap();
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&path) {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if cancel_caller {
            let task = tokio::spawn(async move { p.next_event().await });
            task.abort();
            let _ = task.await;
        } else {
            drop(p);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        #[cfg(windows)]
        let alive = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .unwrap()
            .stdout;
        #[cfg(unix)]
        let alive = std::process::Command::new("ps")
            .args(["-p", pid.trim(), "-o", "pid="])
            .output()
            .unwrap()
            .stdout;
        assert!(!String::from_utf8_lossy(&alive)
            .split_whitespace()
            .any(|s| s == pid.trim()));
    }
}

#[tokio::test]
async fn descriptor_contract_bounds_and_deadline() {
    for (provider, error) in [
        ("describe_oversize", AuthError::Limit),
        ("describe_wrong", AuthError::Identity),
        ("describe_no_rpc", AuthError::Protocol),
        ("describe_exit", AuthError::Exit),
        ("hang", AuthError::Timeout),
    ] {
        let start = std::time::Instant::now();
        assert_eq!(describe(&options(provider)).await, Err(error));
        assert!(start.elapsed() < Duration::from_secs(8));
    }
}

#[tokio::test]
async fn framing_identity_and_result_fail_closed() {
    for (provider, ready, expected) in [
        ("invalid_utf8", false, AuthError::Protocol),
        ("unterminated", false, AuthError::Protocol),
        ("over_boundary", true, AuthError::Limit),
        ("empty_id", true, AuthError::Protocol),
        ("missing_result", true, AuthError::Protocol),
    ] {
        let mut p = AuthProcess::spawn(options(provider)).unwrap();
        if ready {
            assert!(matches!(
                p.next_event().await.unwrap(),
                Some(AuthEvent::Ready(_))
            ));
        }
        assert_eq!(p.next_event().await.unwrap_err(), expected, "{provider}");
        p.shutdown().await.unwrap();
    }
    for (provider, action, expected) in [
        ("false_zero", AuthAction::Decline, AuthError::Exit),
        ("invalid_result", AuthAction::Accept, AuthError::Protocol),
        ("duplicate_id", AuthAction::Accept, AuthError::Protocol),
    ] {
        let mut p = AuthProcess::spawn(options(provider)).unwrap();
        prompt(&mut p).await;
        p.respond(response("one", action)).await.unwrap();
        assert_eq!(p.next_event().await.unwrap_err(), expected, "{provider}");
        p.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn camel_case_utf8_redaction_and_exact_line_limit() {
    for provider in ["url", "boundary"] {
        let mut p = AuthProcess::spawn(options(provider)).unwrap();
        assert!(matches!(
            p.next_event().await.unwrap(),
            Some(AuthEvent::Ready(_))
        ));
        let event = p.next_event().await.unwrap().unwrap();
        if let AuthEvent::Interaction(i) = &event {
            assert_eq!(i.request_id, "one");
            if provider == "url" {
                assert_eq!(i.mode, AuthMode::Url);
                assert!(i.message.contains("🔐 café"));
                assert_eq!(i.elicitation_id.as_deref(), Some("SECRET"));
                assert!(i.requested_schema.is_some());
                assert!(!format!("{event:?}").contains("SECRET"));
            }
        } else {
            panic!("expected interaction");
        }
        p.respond(response("one", AuthAction::Accept))
            .await
            .unwrap();
        assert!(matches!(
            p.next_event().await.unwrap(),
            Some(AuthEvent::Result(AuthResult {
                authenticated: true,
                ..
            }))
        ));
        p.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn response_phase_identity_size_and_write_deadline() {
    let mut p = AuthProcess::spawn(options("pre_prompt")).unwrap();
    assert!(matches!(
        p.next_event().await.unwrap(),
        Some(AuthEvent::Ready(_))
    ));
    assert_eq!(
        p.respond(response("one", AuthAction::Accept)).await,
        Err(AuthError::Identity)
    );
    assert!(matches!(
        p.next_event().await.unwrap(),
        Some(AuthEvent::Interaction(_))
    ));
    p.respond(response("one", AuthAction::Accept))
        .await
        .unwrap();
    p.shutdown().await.unwrap();
    for (provider, size, error) in [("normal", 65536, AuthError::Limit)] {
        let mut opts = options(provider);
        opts.prompt_timeout = Duration::from_millis(300);
        let mut p = AuthProcess::spawn(opts).unwrap();
        prompt(&mut p).await;
        let mut r = response("one", AuthAction::Accept);
        r.content = Some(serde_json::Map::from_iter([(
            "key".into(),
            "S".repeat(size).into(),
        )]));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), p.respond(r))
                .await
                .unwrap(),
            Err(error)
        );
        assert_eq!(p.next_event().await.unwrap_err(), error);
        p.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn tree_cleanup_and_cancelled_shutdown() {
    for drop_owner in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pid");
        let parent = dir.path().join("pid.parent");
        let mut opts = options("tree");
        opts.env
            .insert("AUTH_FIXTURE_PID".into(), path.as_os_str().into());
        let mut p = AuthProcess::spawn(opts).unwrap();
        let pids = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let (Ok(child), Ok(parent)) = (
                    std::fs::read_to_string(&path),
                    std::fs::read_to_string(&parent),
                ) {
                    if !child.is_empty() && !parent.is_empty() {
                        break [child, parent];
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if drop_owner {
            drop(p);
        } else {
            // Cancellation of a shutdown waiter must not cancel cleanup ownership.
            let _ = tokio::time::timeout(Duration::from_millis(1), p.shutdown()).await;
            p.shutdown().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        for pid in pids {
            #[cfg(windows)]
            let output = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .unwrap();
            #[cfg(unix)]
            let output = std::process::Command::new("ps")
                .args(["-p", pid.trim(), "-o", "pid="])
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .any(|s| s == pid.trim()),
                "surviving PID {pid}"
            );
        }
    }
}
