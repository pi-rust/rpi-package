#![cfg(feature = "test-support")]
use rpi_extcore::{
    host::HostSession,
    mcp::{self, HttpServer, McpConfig, McpOptions, McpOwner, McpServerConfig},
};
use rpi_mcp_core::{AuthDecision, AuthFuture, AuthRequest, CredentialProvider};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};

fn host() -> Arc<HostSession> {
    let (input, output) = tokio::io::duplex(1024 * 1024);
    let (host, events) = HostSession::test_connection_with_ready(
        input,
        128,
        json!({"type":"ready","protocol":"rpi","supportedCommands":["configure_external_tools","external_tool_response"]}),
    );
    tokio::spawn(async move {
        let mut lines = BufReader::new(output).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let _ = events.send(json!({"type":"response","id":request["id"],"success":true}));
        }
    });
    host
}
fn config() -> McpConfig {
    McpConfig {
        scope: "owner-test".into(),
        cwd: std::env::current_dir().unwrap(),
        discovery_timeout: Duration::from_secs(30),
        servers: BTreeMap::from([(
            "http".into(),
            McpServerConfig::Http(HttpServer {
                url: "http://127.0.0.1:1/mcp".into(),
                headers: BTreeMap::new(),
                enabled: true,
                timeout: 30,
                exposure: mcp::Exposure::Direct,
            }),
        )]),
    }
}
#[derive(Default)]
struct DelayedProvider {
    entered: AtomicUsize,
    dropping: AtomicBool,
    release: AtomicBool,
    dropped: AtomicBool,
    calls_after_cancel: AtomicUsize,
}
struct Pending<'a>(&'a DelayedProvider);
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.0.dropping.store(true, Ordering::SeqCst);
        // Simulate delayed resource teardown on MCP's private runtime lane.
        while !self.0.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}
impl CredentialProvider for DelayedProvider {
    fn credential<'a>(&'a self, request: AuthRequest<'a>) -> AuthFuture<'a> {
        Box::pin(async move {
            if self.dropping.load(Ordering::SeqCst) {
                self.calls_after_cancel.fetch_add(1, Ordering::SeqCst);
            }
            let _pending = Pending(self);
            self.entered.fetch_add(1, Ordering::SeqCst);
            request.control.cancelled().await;
            Err(rpi_mcp_core::AuthError::Cancelled)
        })
    }
}
async fn until(check: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_unpublished_attach_and_cancelled_shutdown_retain_join_owner() {
    let host = host();
    let owner = McpOwner::default();
    let provider = Arc::new(DelayedProvider::default());
    let attach = tokio::spawn(mcp::attach_with_options(
        host.clone(),
        config(),
        McpOptions {
            credential_provider: Some(provider.clone()),
            owner: Some(owner.clone()),
            ..Default::default()
        },
    ));
    until(|| provider.entered.load(Ordering::SeqCst) == 1).await;
    attach.abort();
    assert!(matches!(attach.await, Err(error) if error.is_cancelled()));
    until(|| provider.dropping.load(Ordering::SeqCst)).await;
    let shutdown = tokio::spawn({
        let owner = owner.clone();
        async move { owner.shutdown().await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !shutdown.is_finished(),
        "cancellation is not cleanup acknowledgement"
    );
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    let retry = tokio::spawn({
        let owner = owner.clone();
        async move { owner.shutdown().await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!retry.is_finished());
    provider.release.store(true, Ordering::SeqCst);
    assert!(tokio::time::timeout(Duration::from_secs(10), retry)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(provider.dropped.load(Ordering::SeqCst));
    assert_eq!(provider.calls_after_cancel.load(Ordering::SeqCst), 0);
    assert!(mcp::attach_with_options(
        host.clone(),
        config(),
        McpOptions {
            owner: Some(owner.clone()),
            ..Default::default()
        }
    )
    .await
    .err()
    .unwrap()
    .contains("closed"));
    owner.shutdown().await.unwrap();
    host.terminate().await;
}

#[tokio::test]
async fn ready_error_joins_and_empty_config_needs_no_worker() {
    struct Decline(Arc<AtomicBool>);
    impl Drop for Decline {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl CredentialProvider for Decline {
        fn credential<'a>(&'a self, _: AuthRequest<'a>) -> AuthFuture<'a> {
            Box::pin(async { Ok(AuthDecision::Declined) })
        }
    }
    let host = host();
    let owner = McpOwner::default();
    let dropped = Arc::new(AtomicBool::new(false));
    assert!(mcp::attach_with_options(
        host.clone(),
        config(),
        McpOptions {
            credential_provider: Some(Arc::new(Decline(dropped.clone()))),
            owner: Some(owner.clone()),
            ..Default::default()
        }
    )
    .await
    .is_err());
    assert!(
        dropped.load(Ordering::SeqCst),
        "Err must join private runtime/provider teardown"
    );
    let _ = owner.shutdown().await;
    let mut empty = config();
    empty.servers.clear();
    let mut binding = mcp::attach_with_options(
        host.clone(),
        empty,
        McpOptions {
            owner: Some(owner),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(binding.is_finished());
    binding.shutdown().await.unwrap();
    host.terminate().await;
}
