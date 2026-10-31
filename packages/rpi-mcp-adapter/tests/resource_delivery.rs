//! SDK delivery policy exercised against real mock subscriptions and public core batches.
#![forbid(unsafe_code)]
pub use rpi_mcp_adapter::{
    call_result_text, Control, DiscoveredServer, ResourceUpdate, ServerInfo,
};
#[allow(dead_code)]
#[path = "../src/runtime.rs"]
mod facade;
use rpi_mcp_core::{CoreRequestContext, McpRuntime};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

static ENV_LOCK: Mutex<()> = Mutex::new(());
struct SessionEnvGuard(Option<std::ffi::OsString>);
impl SessionEnvGuard {
    fn new(id: &str) -> Self {
        let previous = std::env::var_os(rpi_plugin_sdk::SESSION_ID_ENV);
        std::env::set_var(rpi_plugin_sdk::SESSION_ID_ENV, id);
        Self(previous)
    }
}
impl Drop for SessionEnvGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => std::env::set_var(rpi_plugin_sdk::SESSION_ID_ENV, value),
            None => std::env::remove_var(rpi_plugin_sdk::SESSION_ID_ENV),
        }
    }
}
struct Fixture {
    runtime: McpRuntime,
    context: CoreRequestContext,
    path: PathBuf,
}
impl Fixture {
    fn new(id: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "mcp-delivery-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path,json!({"mcpServers":{"fake":{"command":env!("CARGO_BIN_EXE_fake_mcp_server"),"args":["--stateful-resources","--notify-early","--delivery-values"],"timeout":5}}}).to_string()).unwrap();
        Self {
            runtime: McpRuntime::new(),
            context: CoreRequestContext {
                scope: format!("session:{id}"),
                cwd: std::env::temp_dir(),
            },
            path,
        }
    }
    fn resource(&self, action: &str, uri: &str) {
        self.runtime
            .resources(
                &self.context,
                &json!({"configPath":self.path,"server":"fake","action":action,"uri":uri}),
                &Control::default(),
            )
            .unwrap();
    }
    fn set_value(&self, value: &str) {
        self.runtime.call(&self.context,&json!({"configPath":self.path,"server":"fake","tool":"echo","arguments":{"value":value}}),&Control::default()).unwrap();
    }
    fn wait(&self, count: usize, text: &str) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let batch = self.runtime.take_resource_update_batch(None);
            let ready =
                batch.updates().count() >= count && batch.updates().all(|u| u.text.contains(text));
            let seen: Vec<_> = batch.updates().map(|u| u.text.clone()).collect();
            batch.retry();
            if ready {
                return;
            }
            assert!(
                Instant::now() < end,
                "resource delivery fixture timed out waiting for {count} x {text}: {seen:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn queue(&self, uri: &str, text: &str) {
        // Set the initial value without a notification, then subscribe once.
        // Otherwise subscribe + set_value can queue two reads of the same old
        // value; the second may arrive after flush has leased the first update.
        self.runtime.call(&self.context,&json!({"configPath":self.path,"server":"fake","tool":"echo","arguments":{"value":text,"quiet":true}}),&Control::default()).unwrap();
        self.resource("subscribe", uri);
        self.wait(1, text);
    }
    fn flush(&self, append: impl FnOnce(Value) -> Result<(), String>) -> Result<(), String> {
        facade::flush_resource_updates_with(&self.runtime, append)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.shutdown_connections();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn failed_host_append_preserves_resource_updates_for_the_next_boundary() {
    let _serial = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = SessionEnvGuard::new("delivery-retry");
    let f = Fixture::new("delivery-retry");
    f.queue("mem://delivery", "current content");
    assert!(f.flush(|_| Err("host unavailable".into())).is_err());
    let batch = f.runtime.take_resource_update_batch(None);
    assert_eq!(batch.updates().count(), 1, "failed append lost the update");
    batch.retry();
    let mut delivered = None;
    f.flush(|message| {
        delivered = Some(message);
        Ok(())
    })
    .unwrap();
    let message = delivered.unwrap();
    assert!(message["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("current content"));
    assert_eq!(message["kind"], "custom");
    assert_eq!(message["role"], "custom");
    assert_eq!(
        message["data"],
        json!({"customType":"mcp-resources","display":true})
    );
    assert!(message["timestamp"].is_i64());
    assert!(message["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("🔔 MCP resource update(s):\n\n--- mem://delivery (server "));
    f.flush(|_| panic!("successfully delivered update was replayed"))
        .unwrap();
}

#[test]
fn failed_delivery_cannot_revive_retired_or_superseded_resource_content() {
    let _serial = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = SessionEnvGuard::new("delivery-lifetime");
    for action in [
        "unsubscribe",
        "close",
        "replace",
        "newer-read",
        "newer-delivered",
    ] {
        let f = Fixture::new("delivery-lifetime");
        const URI: &str = "mem://lifetime";
        f.queue(URI, "obsolete content");
        let mut delivered = Vec::new();
        let result = f.flush(|_| {
            match action {
                "unsubscribe" => f.resource("unsubscribe", URI),
                "close" => f.runtime.shutdown_connections(),
                _ => {
                    if action == "replace" {
                        f.resource("unsubscribe", URI);
                        f.queue(URI, "fresh content");
                    } else {
                        // Same subscription lifetime: revision, not pointer identity,
                        // must reject this older failed delivery.
                        f.set_value("fresh content");
                        f.wait(1, "fresh content");
                    }
                    if action == "newer-delivered" {
                        f.flush(|message| {
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
        f.flush(|message| {
            delivered.push(message);
            Ok(())
        })
        .unwrap();
        if matches!(action, "unsubscribe" | "close") {
            assert!(delivered.is_empty(), "{action}: {delivered:?}");
        } else {
            assert_eq!(delivered.len(), 1, "{action}: {delivered:?}");
            let text = delivered[0]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("fresh content"), "{action}: {text}");
            assert!(!text.contains("obsolete content"), "{action}: {text}");
        }
    }
}

#[test]
fn failed_delivery_drops_updates_after_a_session_switch() {
    let _serial = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = SessionEnvGuard::new("delivery-a");
    let f = Fixture::new("delivery-a");
    f.queue("mem://session", "A content");
    // A second scope on the same caller-owned runtime, without touching A's connection.
    let context_b = CoreRequestContext {
        scope: "session:delivery-b".into(),
        cwd: f.context.cwd.clone(),
    };
    assert!(f.flush(|_| {
        std::env::set_var(rpi_plugin_sdk::SESSION_ID_ENV,"delivery-b");
        f.runtime.call(&context_b,&json!({"configPath":f.path,"server":"fake","tool":"echo","arguments":{"value":"B content","quiet":true}}),&Control::default()).unwrap();
        f.runtime.resources(&context_b,&json!({"configPath":f.path,"server":"fake","action":"subscribe","uri":"mem://b"}),&Control::default()).unwrap();
        f.wait(1,"B content");
        Err("host switched session".into())
    }).is_err());
    let updates = f.runtime.take_resource_updates(None);
    assert_eq!(updates.len(), 1, "old session was requeued: {updates:?}");
    assert!(updates[0].text.contains("B content"));
}

#[test]
fn failed_delivery_keeps_the_queue_bounded_and_prefers_new_updates() {
    let _serial = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = SessionEnvGuard::new("delivery-bounds");
    let f = Fixture::new("delivery-bounds");
    f.queue("mem://old", "old content");
    assert!(f.flush(|_| {
        f.runtime.call(&f.context,&json!({"configPath":f.path,"server":"fake","tool":"echo","arguments":{"value":"fresh content","quiet":true}}),&Control::default()).unwrap();
        for index in 0..64 {f.resource("subscribe",&format!("mem://fresh-{index}"));}
        f.wait(64,"fresh content");
        Err("host unavailable".into())
    }).is_err());
    let updates = f.runtime.take_resource_updates(None);
    assert_eq!(updates.len(), 64);
    assert!(updates
        .iter()
        .all(|update| update.text.contains("fresh content")));
}

#[test]
fn hot_switch_without_an_mcp_call_does_not_deliver_previous_session_updates() {
    let _serial = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = SessionEnvGuard::new("a");
    let mut f = Fixture::new("a");
    f.queue("mem://secret", "A secret");
    // Seed only the adapter's older-embedder fallback scope.
    let _ = facade::resources(&json!({"__rpi":{"sessionId":"a"}}), &Control::default());
    std::env::set_var(rpi_plugin_sdk::SESSION_ID_ENV, "b");
    let scope = facade::current_scope();
    assert_eq!(scope.as_deref(), Some("session:b"));
    assert!(f.runtime.take_resource_updates(scope.as_deref()).is_empty());
    assert!(f
        .runtime
        .take_resource_updates(Some("session:a"))
        .is_empty());
    f.context.scope = "session:b".into();
    f.queue("mem://b", "B");
    let served = f
        .runtime
        .take_resource_updates(facade::current_scope().as_deref());
    assert_eq!(served.len(), 1);
    assert!(served[0].text.contains('B'));
}

#[test]
fn invalid_current_session_never_reuses_cached_scope() {
    let _serial = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = SessionEnvGuard::new("a");
    let f = Fixture::new("a");
    f.queue("mem://a", "A");
    let _ = facade::resources(&json!({"__rpi":{"sessionId":"a"}}), &Control::default());
    std::env::set_var(rpi_plugin_sdk::SESSION_ID_ENV, "  ");
    assert!(f
        .runtime
        .take_resource_updates(facade::current_scope().as_deref())
        .is_empty());
    std::env::remove_var(rpi_plugin_sdk::SESSION_ID_ENV);
    assert_eq!(facade::current_scope().as_deref(), Some("session:a"));
}
