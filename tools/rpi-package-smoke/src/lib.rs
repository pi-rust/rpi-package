#[cfg(test)]
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
        let release_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("release");
        let session = load_session(&[release_dir], Arc::new(NullDiagnostics), None);
        let snapshot = session.snapshot().expect("package registry should exist");
        let names: BTreeSet<&str> = snapshot
            .tools()
            .iter()
            .map(|tool| tool.tool.name.as_str())
            .collect();
        let expected = BTreeSet::from([
            "background_task",
            "code_lens",
            "delegate_task",
            "mcp_request",
            "web_fetch",
            "codegraph",
            "memory",
            "todo",
            "token_count",
            "ask_user",
            "permissions",
            "simplify",
            "search",
            "goal",
            "websearch",
            "webfetch",
            "firecrawl_search",
            "firecrawl_scrape",
            "extension_rpc_client",
            "extension_rpc_server",
            "im_message_server",
        ]);
        assert_eq!(names, expected);
        assert_eq!(
            session.loaded_paths().len(),
            19,
            "all nineteen cdylibs should load"
        );
        assert_eq!(
            snapshot.renderers().len(),
            1,
            "token renderer should register"
        );
        let renderer = &snapshot.renderers()[0];
        let input = r#"{"usage":{"input":1200,"output":300,"totalTokens":1500}}"#;
        let mut rendered = StbString::empty();
        let rc = (renderer.render_fn)(
            StbStringRef::from_str(input),
            &mut rendered as *mut StbString,
            renderer.user_data,
        );
        assert_eq!(rc, 0, "token renderer should accept usage payload");
        assert!(rendered.to_string_lossy().contains("1.2k"));
        rendered.free_with(Some(renderer.plugin_free_string));

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

        let session = load_session_mixed(
            &[],
            &[PathBuf::from(&dll)],
            Arc::new(NullDiagnostics),
            None,
        );
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
            let text = out.to_string_lossy();
            assert!(text.contains(needle), "/voice {args} -> {text}");
            eprintln!("/voice {args}: {text}");
            host_free_string(out);
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
        // SAFETY: tag == MessageEnd, so the `message` arm is the live variant.
        let payload = unsafe { event.payload.message.message };
        host_free_string(payload);
    }
}
