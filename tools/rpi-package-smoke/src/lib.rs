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
        ]);
        assert_eq!(names, expected);
        assert_eq!(
            session.loaded_paths().len(),
            18,
            "all eighteen cdylibs should load"
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
}
