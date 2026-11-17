#![deny(unsafe_code)]

// The integration tests drive host-side ABI trampolines directly: they must read
// a plugin-produced `StbString` and release it exactly once, which the SDK
// exposes as explicit `unsafe` operations. Library code stays safe; each test
// module that touches the ABI opts in locally.

#[cfg(test)]
#[allow(unsafe_code)] // host-side ABI reads/releases in this test module
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rpi_agent::{AgentTool, TextContentOrImage};
    use rpi_extensions::{load_session, NullDiagnostics, PluginToolAdapter};
    use rpi_plugin_sdk::{StbString, StbStringRef};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    #[ignore = "run with `task smoke` after the release cdylibs are built"]
    async fn release_plugins_load_and_execute() {
        // Load the workspace's own cdylibs, not every DLL in `target/release`:
        // the tests key off the exact tool set, so a stale artifact from another
        // branch must not be able to change the result. `task smoke` stages the
        // directory; fall back to the build dir for a direct `cargo test`.
        let release_dir = std::env::var_os("RPI_SMOKE_PLUGIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join("target")
                    .join("release")
            });
        let session = load_session(&[release_dir], Arc::new(NullDiagnostics), None);
        let snapshot = session.snapshot().expect("package registry should exist");
        let names: BTreeSet<&str> = snapshot
            .tools()
            .iter()
            .map(|tool| tool.tool.name.as_str())
            .collect();
        let expected = BTreeSet::from([
            "ask_user",
            "code_lens",
            "codegraph_callees",
            "codegraph_callers",
            "codegraph_explore",
            "codegraph_files",
            "codegraph_impact",
            "codegraph_node",
            "codegraph_search",
            "codegraph_status",
            "delegate_task",
            "dispatch",
            "goal",
            "im_message_server",
            "langfuse_prompt",
            "langfuse_score",
            "langfuse_trace",
            "mcp_call",
            "mcp_list",
            "mcp_request",
            "mcp_resource",
            "permissions",
            "plan",
            "plan_mode_complete",
            "plan_mode_start",
            "rpc_client",
            "rpc_server",
            "search",
            "subagent",
            "todo",
            "webfetch",
            "websearch",
            "zoom",
        ]);
        assert_eq!(names, expected);
        assert_eq!(
            session.loaded_paths().len(),
            20,
            "all twenty cdylibs should load"
        );
        let tool = snapshot
            .tools()
            .iter()
            .find(|tool| tool.tool.name == "delegate_task")
            .expect("delegate_task should be registered");
        let adapter = PluginToolAdapter::new(tool.tool.clone(), tool.handle(), session.keepalive());
        let result = adapter
            .execute(
                "smoke-1",
                serde_json::json!({"task":"review the package workspace","maxTurns":4}),
                CancellationToken::new(),
                Arc::new(|_| {}),
            )
            .await
            .expect("delegate_task should execute through the ABI bridge");
        let TextContentOrImage::Text(text) = &result.content[0] else {
            panic!("delegate_task should return text");
        };
        assert!(text.text.contains("rpi-delegation"));
        assert!(text.text.contains("review the package workspace"));
    }

    /// Loads a single built `rpi_voice` cdylib and checks the command/event
    /// wiring end to end (no mic, no audio). Point `RPI_VOICE_DLL` at the built
    /// artifact to run; otherwise it is a no-op.
    #[tokio::test]
    async fn voice_extension_registers_command_and_event() {
        use rpi_extensions::{host_free_string, load_session_mixed};
        use rpi_plugin_sdk::{EventTag, StablePluginEvent};

        let Ok(dll) = std::env::var("RPI_VOICE_DLL") else {
            eprintln!("RPI_VOICE_DLL not set; skipping voice integration test");
            return;
        };
        // Keep any stray MessageEnd from synthesizing/playing audio.
        std::env::set_var("RPI_VOICE_AUTO_TTS", "off");

        let session =
            load_session_mixed(&[], &[PathBuf::from(&dll)], Arc::new(NullDiagnostics), None);
        let snapshot = session.snapshot().expect("voice extension should register");

        let cmd = snapshot
            .commands()
            .iter()
            .find(|c| c.name == "voice")
            .expect("/voice command should be registered");

        // Synchronous subcommands return a TUI message payload.
        for (args, needle) in [("status", "rpi-voice"), ("model", "STT")] {
            let mut out = StbString::empty();
            let payload = format!(r#"{{"args":"{args}","command":"/voice"}}"#);
            let rc = (cmd.handler)(
                StbStringRef::from_str(&payload),
                &mut out as *mut StbString,
                cmd.user_data,
            );
            assert_eq!(rc, 0, "/voice {args} rc");
            // SAFETY: the callback returned a live SDK-owned allocation; copy
            // it before freeing it exactly once with the host allocator.
            let text = unsafe { out.to_string_lossy() };
            assert!(text.contains(needle), "/voice {args} -> {text}");
            eprintln!("/voice {args}: {text}");
            unsafe { host_free_string(out) };
        }

        let handlers = snapshot.handlers_for(EventTag::MessageEnd);
        assert!(
            !handlers.is_empty(),
            "MessageEnd handler should be registered"
        );

        // Dispatch a real MessageEnd so the handler body runs (TTS off → no audio).
        let message = StbString::from_string(
            serde_json::json!({
                "role": "assistant",
                "kind": "assistant",
                "content": [{"type": "text", "text": "integration test"}]
            })
            .to_string(),
        );
        let event = StablePluginEvent::message(EventTag::MessageEnd, message);
        let handler = &handlers[0];
        let rc = (handler.handler)(event, handler.user_data);
        assert_eq!(rc, 0, "MessageEnd handler rc");
        // SAFETY: the handler borrows `message`; it is still live and owned by
        // this test, which releases it exactly once here.
        unsafe { host_free_string(message) };
    }
}

/// Fixture lookup for the `--ignored` smoke tests; see the module for why a
/// missing fixture skips rather than fails. Also compiled into
/// `tests/run_stats.rs`, which is why it lives in its own file.
#[cfg(test)]
#[path = "fixtures.rs"]
pub(crate) mod smoke_fixtures;

#[cfg(test)]
mod mcp_smoke;

#[cfg(test)]
mod todo_smoke;

#[cfg(test)]
#[allow(unsafe_code)] // host-side ABI reads/releases in this test module
mod plan_smoke;

#[cfg(test)]
#[allow(unsafe_code)] // host-side ABI reads/releases in this test module
mod pet_smoke;

#[cfg(test)]
#[allow(unsafe_code)] // host-side ABI reads/releases in this test module
mod stats_smoke;
