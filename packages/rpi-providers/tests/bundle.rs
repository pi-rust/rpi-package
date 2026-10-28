//! One DLL must register and route every provider without sharing credentials.
use rpi_ai::{
    types::{Content, Message, UserMessage},
    Context, Provider, SimpleStreamOptions,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn load() -> (rpi_extensions::ExtensionSession, Vec<Arc<dyn Provider>>) {
    let profile = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let filename = format!(
        "{}rpi_providers{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let library = [profile.join(&filename), profile.join("deps").join(filename)]
        .into_iter()
        .find(|path| path.is_file())
        .expect("first cargo build -p rpi-providers");
    let session = rpi_extensions::load_session_mixed(
        &[],
        &[library],
        Arc::new(rpi_extensions::NullDiagnostics),
        None,
    );
    let providers = rpi_extensions::PluggableProvider::from_session(
        &session,
        tokio::runtime::Handle::current(),
    );
    (session, providers)
}

#[tokio::test]
async fn one_library_declares_all_provider_catalogs_and_auth_policies() {
    let (session, providers) = load();
    let snapshot = session.snapshot().expect("bundle loaded");
    assert_eq!(providers.len(), 5);
    assert_eq!(snapshot.providers().len(), 5);
    // Antigravity authenticates through its OAuth flow only; it declares no API-key variable.
    for (id, env, auth) in [
        ("deepseek", Some("DEEPSEEK_API_KEY"), true),
        ("openrouter", Some("OPENROUTER_API_KEY"), true),
        ("llama-cpp", Some("LLAMA_CPP_API_KEY"), false),
        ("proxy", Some("PROXY_API_KEY"), false),
        ("antigravity", None, true),
    ] {
        let manifest = snapshot
            .provider_manifest(id)
            .expect("manifest in same plugin");
        assert_eq!(manifest.api_key_env.as_deref(), env);
        assert_eq!(manifest.requires_auth, auth);
        assert!(!manifest.models.is_empty());
        assert!(manifest
            .models
            .iter()
            .all(|model| model.provider == id && model.headers.is_none()));
        assert!(snapshot
            .providers()
            .iter()
            .any(|provider| provider.provider_id == id && provider.stream_fn.is_some()));
        assert!(providers.iter().any(|provider| provider.id() == id));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_requests_use_each_providers_own_factory_and_credentials() {
    let (_session, providers) = load();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut seen = vec![];
        for _ in 0..4 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            let end = loop {
                let mut bytes = [0; 4096];
                let count = socket.read(&mut bytes).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&bytes[..count]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break end;
                    }
                }
            };
            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
            let body: serde_json::Value = serde_json::from_slice(&request[end + 4..]).unwrap();
            let id = match body["model"].as_str().unwrap() {
                "deepseek-v4-pro" => "deepseek",
                "openrouter/auto" => "openrouter",
                "local" => "llama-cpp",
                "default" => "proxy",
                other => panic!("unexpected model {other}"),
            };
            assert!(headers.starts_with("post /v1/chat/completions "));
            assert!(headers.contains(&format!("authorization: bearer fixture-{id}")));
            for (name, expected) in [
                ("authorization", format!("bearer fixture-{id}")),
                ("x-provider-fixture", id.to_string()),
            ] {
                let values: Vec<_> = headers
                    .lines()
                    .filter_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case(name).then(|| value.trim())
                    })
                    .collect();
                assert_eq!(values, [expected.as_str()], "{id}: {name}");
            }
            assert!(!headers.contains("stale-"));
            assert_eq!(
                headers.contains("x-title: request title"),
                id == "openrouter"
            );
            if id == "openrouter" {
                assert!(headers.contains("http-referer: https://fixture.invalid/"));
                assert!(!headers.contains("x-title: rpi"));
                assert!(!headers.contains("x-title: fixture title"));
            }
            if id == "deepseek" {
                assert_eq!(body["thinking"]["type"], "disabled");
            }
            assert!(!seen.contains(&id));
            seen.push(id);
            let chunk = serde_json::json!({"choices":[{"index":0,"delta":{"content":id},"finish_reason":"stop"}]});
            let reply = format!("data: {chunk}\n\ndata: [DONE]\n\n");
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.unwrap();
        }
    });
    let mut requests = vec![];
    // Antigravity speaks Cloud Code's Gemini SSE, not this chat server; it is covered by
    // tests/antigravity.rs, so the four chat providers keep this shared exchange unchanged.
    for provider in providers
        .into_iter()
        .filter(|provider| provider.id() != rpi_providers::antigravity::PROVIDER_ID)
    {
        let mut model = provider.models()[0].clone();
        model.base_url = base.clone();
        model.headers = Some(std::collections::BTreeMap::from([
            ("X-Provider-Fixture".into(), "model-default".into()),
            ("Authorization".into(), "Bearer stale-model".into()),
        ]));
        if provider.id() == "openrouter" {
            let headers = model.headers.as_mut().unwrap();
            headers.insert("x-title".into(), "Fixture title".into());
            headers.insert("http-referer".into(), "https://fixture.invalid/".into());
        }
        requests.push(tokio::spawn(async move {
            let mut headers = std::collections::BTreeMap::from([
                ("x-provider-fixture".into(), provider.id().to_string()),
                ("aUtHoRiZaTiOn".into(), "Bearer stale-request".into()),
            ]);
            if provider.id() == "openrouter" {
                headers.insert("X-Title".into(), "Request title".into());
            }
            let options = SimpleStreamOptions {
                api_key: Some(format!("fixture-{}", provider.id())),
                headers: Some(headers),
                max_retries: Some(0),
                timeout: Some(Duration::from_secs(5)),
                ..Default::default()
            };
            let context = Context::new(vec![Message::User(UserMessage::new("hello", 0))]);
            let mut stream = provider.stream_simple(&model, &context, &options).await;
            let mut terminals = 0;
            while let Some(event) = stream.next().await {
                if event.is_terminal() {
                    terminals += 1;
                }
            }
            let result = stream.result().await.unwrap();
            assert_eq!(result.stop_reason, rpi_ai::StopReason::Stop);
            assert_eq!(result.provider, provider.id());
            assert_eq!(Content::text_only(&result.content, ""), provider.id());
            assert_eq!(terminals, 1);
        }));
    }
    for request in requests {
        tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .unwrap()
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
