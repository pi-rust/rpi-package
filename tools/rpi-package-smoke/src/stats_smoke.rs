use rpi_extensions::{
    dispatch_data_event_claiming, dispatch_empty_event, host_free_string,
    load_session_mixed, NullDiagnostics, RegistrySnapshot,
};
use rpi_plugin_sdk::{EventTag, StablePluginEvent, StbString, StbStringRef};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

struct Shutdown(Arc<RegistrySnapshot>);
impl Drop for Shutdown {
    fn drop(&mut self) {
        dispatch_empty_event(&self.0, EventTag::SessionShutdown);
    }
}

#[test]
#[ignore = "set RPI_STATS_DLL to the installed monitor DLL"]
fn installed_monitor_loads_commands_and_events_and_shuts_down_cleanly() {
    let dll = PathBuf::from(std::env::var("RPI_STATS_DLL").unwrap());
    let session = load_session_mixed(&[], &[dll], Arc::new(NullDiagnostics), None);
    let snapshot = session.snapshot_arc().expect("monitor DLL should load");
    let _shutdown = Shutdown(snapshot.clone());
    assert!(snapshot.has_shortcut("f8"));
    for kind in ["press", "repeat", "release"] {
        assert!(dispatch_data_event_claiming(
            &snapshot,
            EventTag::Input,
            &json!({"type":"key","key":"f8","kind":kind}).to_string()
        ));
    }
    assert!(!dispatch_data_event_claiming(
        &snapshot,
        EventTag::Input,
        &json!({"type":"key","key":"f8","kind":"press","ctrl":true}).to_string()
    ));
    let cmd = snapshot
        .commands()
        .iter()
        .find(|c| c.name == "stats")
        .expect("stats command registered");
    for (args, needle) in [
        ("on", "已打开"),
        ("position bottom-left", "已更新"),
        ("off", "已关闭"),
    ] {
        let input = json!({"command":"stats","args":args}).to_string();
        let mut out = StbString::empty();
        assert_eq!(
            (cmd.handler)(StbStringRef::from_str(&input), &mut out, cmd.user_data),
            0
        );
        let text = out.to_string_lossy();
        host_free_string(out);
        assert!(text.contains(needle), "{args}: {text}");
    }
    for tag in [
        EventTag::SessionStart,
        EventTag::AgentStart,
        EventTag::AgentEnd,
    ] {
        assert!(!snapshot.handlers_for(tag).is_empty());
        dispatch_empty_event(&snapshot, tag);
    }
    for (tag, message) in [
        (EventTag::BeforeProviderRequest, json!({})),
        (
            EventTag::MessageUpdate,
            json!({"assistantMessageEvent":{"type":"text_delta","delta":"hello"}}),
        ),
        (
            EventTag::MessageEnd,
            json!({"role":"assistant","usage":{"input":100,"output":252}}),
        ),
    ] {
        let handlers = snapshot.handlers_for(tag);
        assert_eq!(handlers.len(), 1);
        let payload = StbString::from_string(message.to_string());
        let event = if tag == EventTag::BeforeProviderRequest {
            StablePluginEvent::data(tag, payload)
        } else {
            StablePluginEvent::message(tag, payload)
        };
        // Match host fan-out ownership: handlers borrow the event, then the
        // host frees its allocation once the handlers have returned.
        assert_eq!((handlers[0].handler)(event,handlers[0].user_data),0);
        host_free_string(payload);
    }
}
