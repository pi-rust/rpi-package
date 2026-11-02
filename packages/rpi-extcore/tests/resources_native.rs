//! Opt-in trusted-native integration. No PATH executable is accepted.
//! Run this binary alone with the documented isolated environment.
use rpi_extcore::{
    reply::ReplyKind,
    resources::{local::*, *},
    service::{self, BackendConfig, CreateSessionRequest, Service},
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[path = "support/resource_fork_core.rs"]
mod resource_fork_core;
#[path = "support/resource_saved_core.rs"]
mod resource_saved_core;
fn put(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
fn selection(instruction: &str, skill: &str, library: &Path, tools: Value) -> ResourceSelection {
    serde_json::from_value(json!({"version":1,"instructions":[instruction],"skills":[skill],"extensions":[library],"tools":tools})).unwrap()
}
async fn request_body(socket: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut buf = [0; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        assert!(bytes.len() < 512 * 1024);
    };
    let header = String::from_utf8_lossy(&bytes[..header_end]);
    let length: usize = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim().parse().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + length {
        let mut buf = [0; 4096];
        let n = socket.read(&mut buf).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
}
async fn prompt(host: &Arc<rpi_extcore::host::HostSession>, text: &str) {
    let mut run = host.begin_prompt(text.into()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let event = run.events.recv().await.unwrap();
            if let Some(outcome) = run.tracker.observe(&event, false).outcome {
                assert!(outcome.is_ok(), "{outcome:?}");
                break;
            }
        }
    })
    .await
    .unwrap();
}
async fn create(service: &Service, selection: ResourceSelection) -> String {
    let (kind, value) = service::create_session(
        service,
        CreateSessionRequest {
            project_id: None,
            resources: Some(selection),
        },
    )
    .await;
    assert_eq!(kind, ReplyKind::Created, "{value}");
    value["sessionId"].as_str().unwrap().into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires supplied trusted native binary and explicitly built cdylib fixture; see docs"]
async fn trusted_native_resource_service_lifecycle() {
    native_resource_scenario(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires trusted native with new-session fork binding initialization; run explicitly and alone"]
async fn trusted_native_resource_binding_leaf_fork_regression() {
    native_resource_scenario(true).await;
}
async fn native_resource_scenario(check_fork: bool) {
    let bin = PathBuf::from(
        std::env::var_os("RPI_RESOURCE_NATIVE_BIN").expect("explicit native binary required"),
    );
    assert!(bin.is_absolute(), "RPI_RESOURCE_NATIVE_BIN must be an explicit absolute trusted executable path; PATH lookup is forbidden");
    assert!(
        std::fs::metadata(&bin)
            .expect("trusted native binary must exist")
            .is_file(),
        "RPI_RESOURCE_NATIVE_BIN must name a regular file"
    );
    let library = PathBuf::from(
        std::env::var_os("RPI_RESOURCE_TEST_EXTENSION").expect("explicit fixture library required"),
    );
    assert!(bin.is_file() && library.is_file());
    let root = std::env::temp_dir().join(format!("core-resource-native-{}", uuid::Uuid::new_v4()));
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    // This opt-in test process contains only this test. No production API accepts
    // environment, executable or credentials from resource requests.
    std::env::set_var("RPI_CODING_AGENT_DIR", &agent);
    std::env::set_var(
        "RPI_PLUGIN_STUB_REGISTRATION_LOG",
        root.join("registrations.log"),
    );
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
    ] {
        std::env::remove_var(key);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    put(agent.join("models.json"), json!({"providers":{"fixture":{"api":"openai-completions","apiKey":"loopback-only","baseUrl":format!("http://{}/v1",listener.local_addr().unwrap()),"models":[{"id":"test","reasoning":true,"contextWindow":131072,"maxTokens":256},{"id":"alt","reasoning":true,"contextWindow":131072,"maxTokens":256},{"id":"newest","reasoning":true,"contextWindow":131072,"maxTokens":256}]}}}).to_string());
    put(
        agent.join("settings.json"),
        r#"{"defaultProvider":"fixture","defaultModel":"test","retry":{"enabled":false}}"#,
    );
    for label in ["A", "B"] {
        put(
            project.join(format!("{label}.md")),
            format!("CORE_INSTRUCTION_{label}"),
        );
        put(
            project.join(format!("{}/SKILL.md", label.to_ascii_lowercase())),
            format!(
                "---\nname: {}\ndescription: CORE_SKILL_{label}\n---\nbody",
                label.to_ascii_lowercase()
            ),
        );
    }
    // Resource-only legacy Packages are catalog fixtures, never selected ABI
    // resources. Prepare their registry before any host takes resource leases.
    for name in [".tools", "_legacy"] {
        std::fs::create_dir_all(project.join(".rpi/packages").join(name)).unwrap();
    }
    put(
        project.join(".rpi/packages.json"),
        json!({"packages":[
            {"name":".tools","version":"1","source":"fixture-only","cargo_package":"none","artifacts":[]},
            {"name":"_legacy","version":"1","source":"fixture-only","cargo_package":"none","artifacts":[]}
        ]})
        .to_string(),
    );
    let config = BackendConfig {
        project: project.clone(),
        rpi_bin: bin,
        session_dir: Some(root.join("sessions")),
        registry: root.join("registry.json"),
        ..BackendConfig::default()
    };
    let service = Service::new(config.clone()).unwrap();
    let a = selection(
        "A.md",
        "a/SKILL.md",
        &library,
        json!({"mode":"only","names":["read","echo"]}),
    );
    let b = selection(
        "B.md",
        "b/SKILL.md",
        &library,
        json!({"mode":"only","names":["read","echo"]}),
    );
    let none = ResourceSelection {
        version: 1,
        main_package: None,
        skills: vec![],
        extensions: vec![],
        instructions: vec![],
        tools: ResourceTools::None {},
    };
    let aid = create(&service, a.clone()).await;
    let bid = create(&service, b.clone()).await;
    let nid = create(&service, none.clone()).await;
    let ahost = service.live_session(&aid).await.unwrap();
    let bhost = service.live_session(&bid).await.unwrap();
    let b_generation = bhost.resource_state().await.unwrap().generation;
    let server = tokio::spawn(async move {
        for (index, label) in ["A", "A", "B", "B", "B", "B", "none"].iter().enumerate() {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = request_body(&mut socket).await;
            if *label == "none" {
                assert!(
                    request
                        .get("tools")
                        .map_or(true, |v| v.as_array().is_some_and(Vec::is_empty)),
                    "{request}"
                );
            } else {
                let text = request["messages"].to_string();
                assert!(
                    text.contains(&format!("CORE_INSTRUCTION_{label}")),
                    "{request}"
                );
                assert!(text.contains(&format!("CORE_SKILL_{label}")), "{request}");
                let tools = request["tools"].as_array().unwrap();
                assert_eq!(tools.len(), 2, "{tools:?}");
                assert!(tools.iter().any(|t| t["function"]["name"] == "echo"));
            }
            let frame = if index < 6 && index % 2 == 0 {
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call-{index}"),"type":"function","function":{"name":"echo","arguments":"{\"text\":\"CORE_ABI\"}"}}]},"finish_reason":"tool_calls"}]})
            } else {
                if index < 6 {
                    assert!(request["messages"].to_string().contains("echo: CORE_ABI"));
                }
                json!({"choices":[{"index":0,"delta":{"content":"core real reply"},"finish_reason":"stop"}]})
            };
            let body = format!("data: {frame}\n\ndata: [DONE]\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    prompt(&ahost, "prior real A message").await;
    prompt(&bhost, "prior real B message").await;
    let apply = ApplyResourcesRequest {
        expected_revision: 1,
        operation_id: "A-to-B".into(),
        target: b.clone(),
        supersedes_operation_id: None,
    };
    let result = service
        .apply_resources(aid.clone(), apply.clone())
        .await
        .unwrap();
    assert_eq!(
        result.status,
        ResourceOperationStatus::Applied,
        "{result:?}"
    );
    assert_eq!(result.result_revision, Some(2));
    assert!(ahost
        .begin_prompt("captured retired arc".into())
        .await
        .is_err());
    assert_eq!(
        bhost.resource_state().await.unwrap().generation,
        b_generation
    );
    let replacement = service.live_session(&aid).await.unwrap();
    prompt(&replacement, "after same ID transition").await;
    prompt(&service.live_session(&nid).await.unwrap(), "zero schemas").await;
    tokio::time::timeout(Duration::from_secs(20), server)
        .await
        .unwrap()
        .unwrap();
    let history = replacement
        .command("get_messages", json!({"limit":20}), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        history.to_string().contains("prior real A message")
            && history.to_string().contains("echo: CORE_ABI")
    );
    let generation = replacement.resource_state().await.unwrap().generation;
    let (repeat, concurrent) = tokio::join!(
        service.apply_resources(aid.clone(), apply.clone()),
        service.apply_resources(aid.clone(), apply.clone())
    );
    assert_eq!(repeat.unwrap().result_revision, Some(2));
    assert_eq!(concurrent.unwrap().result_revision, Some(2));
    assert_eq!(
        replacement.resource_state().await.unwrap().generation,
        generation
    );
    let mut different = apply.clone();
    different.target = none.clone();
    assert_eq!(
        service
            .apply_resources(aid.clone(), different)
            .await
            .unwrap_err()
            .kind,
        ResourceErrorKind::Conflict
    );
    let stale = ApplyResourcesRequest {
        operation_id: "stale".into(),
        ..apply
    };
    assert_eq!(
        service
            .apply_resources(aid.clone(), stale)
            .await
            .unwrap_err()
            .kind,
        ResourceErrorKind::Conflict
    );
    let project_id = service::list_projects(&service).await["defaultProjectId"]
        .as_str()
        .unwrap()
        .to_owned();
    let catalog = service
        .manage_resources(LocalResourceRequest {
            project_id: project_id.clone(),
            scope: ResourceScope::Project,
            operation: LocalResourceOperation::List,
        })
        .await
        .unwrap();
    assert!(catalog.success, "{catalog:?}");
    for name in [".tools", "_legacy"] {
        assert!(catalog.observation["data"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "package"
                && r["name"] == name
                && r["ownership"] == "legacy-native"
                && r["installed"] == true));
        let inspected = service
            .manage_resources(LocalResourceRequest {
                project_id: project_id.clone(),
                scope: ResourceScope::Project,
                operation: LocalResourceOperation::Inspect {
                    kind: ResourceKind::Package,
                    name: name.into(),
                },
            })
            .await
            .unwrap();
        assert!(inspected.success, "{inspected:?}");
        let resources = inspected.observation["data"]["resources"]
            .as_array()
            .unwrap();
        assert_eq!(resources.len(), 1, "{inspected:?}");
        assert_eq!(resources[0]["name"], name);
        assert_eq!(resources[0]["ownership"], "legacy-native");
    }
    let released = service
        .release_idle_resources(project_id.clone(), ResourceScope::Project)
        .await
        .unwrap();
    assert_eq!(released.len(), 3);
    // Offline repair must not require opening the now-damaged old selection.
    put(project.join("B.md"), "changed installed instruction");
    let history_without_resources = service
        .resource_saved_history(aid.clone(), ResourceHistoryRequest::default())
        .await
        .unwrap();
    assert!(!history_without_resources.executable);
    assert!(serde_json::to_string(&history_without_resources.messages)
        .unwrap()
        .contains("prior real A message"));
    assert_ne!(
        service::open_session(&service, aid.clone()).await.0,
        ReplyKind::Success
    );
    let repair = service
        .apply_resources(
            aid.clone(),
            ApplyResourcesRequest {
                expected_revision: 2,
                operation_id: "explicit-repair".into(),
                target: none.clone(),
                supersedes_operation_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        repair.status,
        ResourceOperationStatus::Applied,
        "{repair:?}"
    );
    assert!(service
        .live_session(&aid)
        .await
        .unwrap()
        .resource_state()
        .await
        .unwrap()
        .body["modelEffectiveTools"]
        .as_array()
        .unwrap()
        .is_empty());
    let settings_host = service.live_session(&aid).await.unwrap();
    settings_host
        .set_model("alt", Some("fixture"))
        .await
        .unwrap();
    settings_host.set_thinking_level("low").await.unwrap();
    service
        .release_idle_resources(project_id.clone(), ResourceScope::Project)
        .await
        .unwrap();
    let saved = service.resource_configuration(&aid).await.unwrap().unwrap();
    assert_eq!(
        saved.settings.model.as_deref(),
        Some("test"),
        "historical apply snapshot is deliberately stale"
    );
    assert_eq!(
        saved
            .current_settings
            .as_ref()
            .unwrap()
            .settings
            .model
            .as_deref(),
        Some("alt")
    );
    let offline = service
        .apply_resources(
            aid.clone(),
            ApplyResourcesRequest {
                expected_revision: saved.revision,
                operation_id: "offline-current-settings".into(),
                target: none.clone(),
                supersedes_operation_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        offline.status,
        ResourceOperationStatus::Applied,
        "{offline:?}"
    );
    assert!(
        service
            .resource_configuration(&aid)
            .await
            .unwrap()
            .unwrap()
            .current_settings
            .is_none(),
        "successful live apply consumes only the stop capture, not operation restore_settings"
    );
    assert_eq!(
        offline.restore_settings.as_ref().unwrap().model.as_deref(),
        Some("alt")
    );
    let current = service
        .live_session(&aid)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(current["modelId"], "alt");
    assert_eq!(current["thinkingLevel"], "low");
    service
        .release_idle_resources(project_id.clone(), ResourceScope::Project)
        .await
        .unwrap();
    assert_eq!(
        service::open_session(&service, aid.clone()).await.0,
        ReplyKind::Success
    );
    let settings_host = service.live_session(&aid).await.unwrap();
    settings_host
        .set_model("newest", Some("fixture"))
        .await
        .unwrap();
    settings_host.set_thinking_level("high").await.unwrap();
    service.shutdown().await;
    let restarted = Service::new(config.clone()).unwrap();
    assert_eq!(
        service::open_session(&restarted, aid.clone()).await.0,
        ReplyKind::Success
    );
    let restored = restarted.live_session(&aid).await.unwrap();
    assert!(
        restored.resource_state().await.unwrap().body["modelEffectiveTools"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(restored
        .command("get_messages", json!({"limit":20}), Duration::from_secs(5))
        .await
        .unwrap()
        .to_string()
        .contains("prior real A message"));
    let current = restored.state().await.unwrap();
    assert_eq!(current["modelId"], "newest");
    assert_eq!(current["thinkingLevel"], "high");
    assert!(
        restarted
            .resource_configuration(&aid)
            .await
            .unwrap()
            .unwrap()
            .current_settings
            .is_none(),
        "a live generation must never retain a stale stop capture"
    );
    restored.set_model("test", Some("fixture")).await.unwrap();
    restored.set_thinking_level("medium").await.unwrap();
    restarted
        .release_idle_resources(project_id.clone(), ResourceScope::Project)
        .await
        .unwrap();
    assert_eq!(
        service::open_session(&restarted, aid.clone()).await.0,
        ReplyKind::Success
    );
    let current = restarted
        .live_session(&aid)
        .await
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(current["modelId"], "test");
    assert_eq!(current["thinkingLevel"], "medium");
    restarted
        .release_idle_resources(project_id.clone(), ResourceScope::Project)
        .await
        .unwrap();
    let restarted = resource_saved_core::verify(&restarted, &aid, &project_id, &config).await;
    println!(
        "isolated native reader/settings integration evidence: {}",
        root.display()
    );
    if !check_fork {
        restarted.shutdown().await;
        return;
    }
    resource_fork_core::verify(&restarted, &aid, &project_id).await;
    restarted.shutdown().await;
}
