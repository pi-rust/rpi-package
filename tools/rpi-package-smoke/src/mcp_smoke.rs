use rpi_agent::{AgentTool, TextContentOrImage};
use rpi_extensions::{
    load_session_mixed, ExtensionSession, NullDiagnostics, PluginToolAdapter, ToolCallContext,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Shutdown(ExtensionSession);
impl Drop for Shutdown {
    fn drop(&mut self) {
        if let Some(snapshot) = self.0.snapshot() {
            rpi_extensions::dispatch_empty_event(
                snapshot,
                rpi_plugin_sdk::EventTag::SessionShutdown,
            );
        }
    }
}
fn text(result: &rpi_agent::types::AgentToolResult) -> &str {
    let TextContentOrImage::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    &text.text
}
#[tokio::test]
#[ignore = "set RPI_MCP_DLL and RPI_FAKE_MCP_SERVER to the built binaries"]
async fn mcp_dll_persistent_scoped_calls_images_and_cancellation() {
    let dll = PathBuf::from(std::env::var("RPI_MCP_DLL").unwrap());
    let fake = std::env::var("RPI_FAKE_MCP_SERVER").unwrap();
    let guard = Shutdown(load_session_mixed(
        &[],
        &[dll],
        Arc::new(NullDiagnostics),
        None,
    ));
    let snapshot = guard.0.snapshot().unwrap();
    let directory = std::env::temp_dir().join(format!("rpi-mcp-abi-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("mcp.json");
    std::fs::write(
        &path,
        json!({"mcpServers":{"fake":{"command":fake,"args":["--paged"],"timeout":2}}}).to_string(),
    )
    .unwrap();
    let adapter = |name: &str, scope: &str| {
        let tool = snapshot
            .tools()
            .iter()
            .find(|t| t.tool.name == name)
            .unwrap();
        let context = ToolCallContext::new(directory.to_string_lossy());
        context.set_session_id(scope);
        PluginToolAdapter::new(tool.tool.clone(), tool.handle(), guard.0.keepalive())
            .with_context(context)
    };
    let call_a = adapter("mcp_call", "A");
    let call_b = adapter("mcp_call", "B");
    let list = adapter("mcp_list", "A");
    let run = |tool: PluginToolAdapter, params: Value| async move {
        tool.execute(
            "abi-smoke",
            params,
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await
    };
    let discovered = list
        .execute(
            "list",
            json!({"configPath":"mcp.json","server":"fake"}),
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    let discovered: Value = serde_json::from_str(text(&discovered)).unwrap();
    assert_eq!(discovered["tools"].as_array().unwrap().len(), 4);
    let params = json!({"configPath":"mcp.json","server":"fake","tool":"counter"});
    let first = run(call_a, params.clone()).await.unwrap();
    let second = run(adapter("mcp_call", "A"), params.clone()).await.unwrap();
    let other = run(call_b, params).await.unwrap();
    let (pid, count) = text(&first).split_once(':').unwrap();
    assert_eq!(count, "1");
    assert_eq!(text(&second), format!("{pid}:2"));
    assert_ne!(text(&other).split(':').next().unwrap(), pid);
    let image = run(
        adapter("mcp_call", "A"),
        json!({"configPath":"mcp.json","server":"fake","tool":"image"}),
    )
    .await
    .unwrap();
    let TextContentOrImage::Image(image) = &image.content[0] else {
        panic!("image was flattened to text")
    };
    assert_eq!(image.data, "aGVsbG8=");
    assert_eq!(image.mime_type, "image/png");
    assert!(run(
        adapter("mcp_call", "A"),
        json!({"configPath":"mcp.json","server":"fake","tool":"fail"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("intentional failure"));
    std::fs::write(
        &path,
        json!({"mcpServers":{"fake":{"command":fake,"args":["--hang-call"],"timeout":60}}})
            .to_string(),
    )
    .unwrap();
    let token = CancellationToken::new();
    let cancelling = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cancelling.cancel();
    });
    let start = std::time::Instant::now();
    let result = adapter("mcp_call", "A")
        .execute(
            "cancel",
            json!({"configPath":"mcp.json","server":"fake","tool":"echo"}),
            token,
            Arc::new(|_| {}),
        )
        .await;
    assert!(result.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    drop(guard);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
#[ignore = "set RPI_MCP_DLL and RPI_REAL_MCP_CONFIG for an actual Playwright probe"]
async fn mcp_dll_real_playwright_probe() {
    let dll = PathBuf::from(std::env::var("RPI_MCP_DLL").unwrap());
    let config = std::env::var("RPI_REAL_MCP_CONFIG").unwrap();
    let guard = Shutdown(load_session_mixed(
        &[],
        &[dll],
        Arc::new(NullDiagnostics),
        None,
    ));
    let snapshot = guard.0.snapshot().unwrap();
    let adapter = |name: &str| {
        let tool = snapshot
            .tools()
            .iter()
            .find(|t| t.tool.name == name)
            .unwrap();
        PluginToolAdapter::new(tool.tool.clone(), tool.handle(), guard.0.keepalive())
    };
    let tools = adapter("mcp_list")
        .execute(
            "real-list",
            json!({"configPath":config,"server":"playwright","timeoutSeconds":5}),
            CancellationToken::new(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    let tools: Value = serde_json::from_str(text(&tools)).unwrap();
    assert!(tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "browser_tabs"));
    eprintln!(
        "Real Playwright discovery: {} tools",
        tools["tools"].as_array().unwrap().len()
    );
    let start = std::time::Instant::now();
    let result = adapter("mcp_call").execute("real-call", json!({"configPath":config,"server":"playwright","tool":"browser_tabs","arguments":{"action":"list"},"timeoutSeconds":3}), CancellationToken::new(), Arc::new(|_| {})).await;
    match result {
        Ok(result) => eprintln!(
            "Real Playwright browser connection succeeded: {}",
            text(&result)
        ),
        Err(error) => {
            assert!(
                error.to_string().contains("tools/call timed out"),
                "{error}"
            );
            eprintln!("Real Playwright browser connection still pending; bounded timeout verified: {error}");
        }
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

#[tokio::test]
#[ignore = "requires a configured Playwright extension token and an open Chrome profile"]
async fn mcp_dll_real_playwright_authenticated_browser() {
    let dll = PathBuf::from(std::env::var("RPI_MCP_DLL").unwrap());
    let config = std::env::var("RPI_REAL_MCP_CONFIG").unwrap();
    let config_value: Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    let secret = config_value["mcpServers"]["playwright"]["env"]["PLAYWRIGHT_MCP_EXTENSION_TOKEN"]
        .as_str()
        .expect("configure the extension token first");
    assert!(!secret.is_empty());
    let guard = Shutdown(load_session_mixed(
        &[],
        &[dll],
        Arc::new(NullDiagnostics),
        None,
    ));
    let snapshot = guard.0.snapshot().unwrap();
    let list_tool = snapshot
        .tools()
        .iter()
        .find(|t| t.tool.name == "mcp_list")
        .unwrap();
    let discovered = PluginToolAdapter::new(
        list_tool.tool.clone(),
        list_tool.handle(),
        guard.0.keepalive(),
    )
    .execute(
        "discover",
        json!({"configPath":config,"server":"playwright"}),
        CancellationToken::new(),
        Arc::new(|_| {}),
    )
    .await
    .unwrap();
    let discovered: Value = serde_json::from_str(text(&discovered)).unwrap();
    eprintln!(
        "Available tools: {}",
        discovered["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let tool = snapshot
        .tools()
        .iter()
        .find(|t| t.tool.name == "mcp_call")
        .unwrap();
    let context = ToolCallContext::new("D:\\Projects\\blogs");
    context.set_session_id("authenticated-browser-smoke");
    let adapter = PluginToolAdapter::new(tool.tool.clone(), tool.handle(), guard.0.keepalive())
        .with_context(context);
    let call = |name: &'static str, arguments: Value| {
        let params = json!({"configPath":config,"server":"playwright","tool":name,"arguments":arguments,"timeoutSeconds":60});
        let adapter = &adapter;
        async move {
            let start = std::time::Instant::now();
            let result = adapter
                .execute(
                    "authenticated-smoke",
                    params,
                    CancellationToken::new(),
                    Arc::new(|_| {}),
                )
                .await;
            eprintln!("{name}: {:.2}s", start.elapsed().as_secs_f64());
            result.unwrap_or_else(|error| {
                panic!("{}", error.to_string().replace(secret, "[REDACTED]"))
            })
        }
    };
    call("browser_tabs", json!({"action":"list"})).await;
    call("browser_tabs", json!({"action":"new"})).await;
    call("browser_evaluate", json!({"function":"() => { document.title = 'RPI MCP smoke'; document.body.innerHTML = '<button id=check>Test connection</button><output id=result>pending</output>'; document.querySelector('#check').addEventListener('click', () => { document.querySelector('#result').textContent = 'RPI_MCP_CLICK_OK'; }); return 'ready'; }"})).await;
    let snapshot = call("browser_snapshot", json!({})).await;
    let button_ref = text(&snapshot)
        .lines()
        .find(|line| line.contains("button") && line.contains("Test connection"))
        .and_then(|line| line.split_once("[ref="))
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(reference, _)| reference.to_owned());
    if let Some(reference) = button_ref.as_ref() {
        call(
            "browser_click",
            json!({"element":"Test connection button","target":reference}),
        )
        .await;
    }
    let result = call("browser_evaluate", json!({"function":"() => ({ title: document.title, result: document.querySelector('#result').textContent })"})).await;
    let clicked =
        text(&result).contains("RPI_MCP_CLICK_OK") && text(&result).contains("RPI MCP smoke");
    call("browser_tabs", json!({"action":"close"})).await;
    assert!(
        button_ref.is_some(),
        "snapshot did not expose the test button"
    );
    assert!(clicked, "browser did not return the expected click result");
    eprintln!("Authenticated browser connection, new tab, click/read, and cleanup passed.");
}

#[tokio::test]
#[ignore = "requires configured Playwright, Chrome, and a screenshot test page"]
async fn mcp_dll_real_playwright_screenshot() {
    let dll = PathBuf::from(std::env::var("RPI_MCP_DLL").unwrap());
    let config = std::env::var("RPI_REAL_MCP_CONFIG").unwrap();
    let config_value: Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    let secret = config_value["mcpServers"]["playwright"]["env"]["PLAYWRIGHT_MCP_EXTENSION_TOKEN"]
        .as_str()
        .unwrap_or("");
    let guard = Shutdown(load_session_mixed(
        &[],
        &[dll],
        Arc::new(NullDiagnostics),
        None,
    ));
    let snapshot = guard.0.snapshot().unwrap();
    let tool = snapshot
        .tools()
        .iter()
        .find(|t| t.tool.name == "mcp_call")
        .unwrap();
    let context = ToolCallContext::new("D:\\Projects\\blogs");
    context.set_session_id("screenshot-smoke");
    let adapter = PluginToolAdapter::new(tool.tool.clone(), tool.handle(), guard.0.keepalive())
        .with_context(context);
    let call = |name: &'static str, arguments: Value| {
        let params = json!({"configPath":config,"server":"playwright","tool":name,"arguments":arguments,"timeoutSeconds":60});
        let adapter = &adapter;
        async move {
            let start = std::time::Instant::now();
            let result = adapter
                .execute(
                    "screenshot-smoke",
                    params,
                    CancellationToken::new(),
                    Arc::new(|_| {}),
                )
                .await;
            eprintln!(
                "{name}: {:.2}s, success={}",
                start.elapsed().as_secs_f64(),
                result.is_ok()
            );
            result.map_err(|error| {
                if secret.is_empty() {
                    error.to_string()
                } else {
                    error.to_string().replace(secret, "[REDACTED]")
                }
            })
        }
    };
    let url = std::env::var("RPI_SCREENSHOT_URL").unwrap_or_else(|_| "about:blank".to_owned());
    let previous_tab = call("browser_tabs", json!({"action":"list"}))
        .await
        .unwrap();
    let current_index = |result: &rpi_agent::types::AgentToolResult| {
        text(result)
            .lines()
            .find(|line| line.contains("(current)"))
            .and_then(|line| line.strip_prefix("- "))
            .and_then(|line| line.split_once(':'))
            .and_then(|(index, _)| index.parse::<u64>().ok())
    };
    let previous_index = current_index(&previous_tab).expect("previous tab index");
    let new_tab = call("browser_tabs", json!({"action":"new","url":url}))
        .await
        .unwrap();
    let tab_index = current_index(&new_tab).expect("new tab index");
    let result = async {
        let state = call("browser_evaluate", json!({"function":"() => ({ url: location.href, ready: document.readyState, images: document.querySelectorAll('article img').length, clickable: document.querySelectorAll('.article-content-image').length, lightbox: !!document.querySelector('.article-image-lightbox') })"})).await?;
        eprintln!("Page state: {}", text(&state));
        if let Ok(target) = std::env::var("RPI_SCREENSHOT_CLICK") {
            call("browser_click", json!({"target":target})).await?;
            let state = call("browser_evaluate", json!({"function":"() => ({ url: location.href, lightboxVisible: !!document.querySelector('.article-image-lightbox:not([hidden])'), preview: document.querySelector('.article-image-lightbox__image')?.currentSrc })"})).await?;
            eprintln!("After click: {}", text(&state));
        }
        let filename = std::env::var("RPI_SCREENSHOT_FILENAME").unwrap_or_else(|_| ".rpi/browser-output/mcp-screenshot-probe.png".to_owned());
        let mut params = json!({"filename":filename});
        if let Ok(target) = std::env::var("RPI_SCREENSHOT_TARGET") { params["target"] = target.into(); }
        if std::env::var("RPI_SCREENSHOT_FULL_PAGE").is_ok() { params["fullPage"] = true.into(); }
        if std::env::var("RPI_SCREENSHOT_FOREGROUND").is_ok() {
            call("browser_tabs", json!({"action":"select","index":tab_index})).await?;
        }
        let screenshot = call("browser_take_screenshot", params).await?;
        eprintln!("Screenshot result: {}", text(&screenshot));
        Ok::<_, String>(())
    }.await;
    if let Err(error) = &result {
        eprintln!("Screenshot error: {error}");
    }
    if std::env::var("RPI_SCREENSHOT_FOREGROUND").is_ok() {
        call(
            "browser_tabs",
            json!({"action":"select","index":previous_index}),
        )
        .await
        .unwrap();
    }
    call("browser_tabs", json!({"action":"close","index":tab_index}))
        .await
        .unwrap();
    assert!(result.is_ok(), "screenshot probe failed");
}
