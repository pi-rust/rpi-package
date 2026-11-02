use super::*;

#[test]
fn native_names_are_bounded_deterministic_and_tuple_distinct() {
    let first = native_name("server name/🙂", "remote.tool/🙂");
    assert_eq!(first.len(), 64);
    assert!(first.starts_with("mcp_"));
    assert!(first[4..].bytes().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(first, native_name("server name/🙂", "remote.tool/🙂"));
    assert_ne!(native_name("ab", "c"), native_name("a", "bc"));
}

#[test]
fn raw_results_are_preserved_and_unsupported_content_is_an_error() {
    let raw = json!({"content":[{"type":"text","text":"raw","annotations":{"priority":1}},{"type":"image","data":"AA==","mimeType":"image/png"}],"isError":true,"structuredContent":{"x":1},"customMetadata":{"opaque":true}});
    assert_eq!(validate_result(raw.clone()).unwrap(), raw);
    for invalid in [
        json!({"content":[{"type":"image","data":"not base64!","mimeType":"image/png"}]}),
        json!({"content":[{"type":"image","data":"AB==","mimeType":"image/png"}]}),
        json!({"content":[{"type":"text","text":"ok","textSignature":123}]}),
        json!({"content":[{"type":"text","text":"ok","textSignature":{}}]}),
        json!({"content":[{"type":"resource","resource":{"text":"unsupported"}}]}),
        json!({"content":[],"isError":"yes"}),
        json!({"content":[],"structuredContent":[]}),
    ] {
        assert!(validate_result(invalid).is_err());
    }
    for signature in [Value::Null, json!("metadata-not-provider-authority")] {
        let raw = json!({"content":[{"type":"text","text":"ok","textSignature":signature,"annotations":{"priority":0.5}}]});
        assert_eq!(validate_result(raw.clone()).unwrap(), raw);
    }
}

#[test]
fn invalid_configuration_does_not_echo_secrets() {
    let config = Configuration {
        cwd: PathBuf::from("C:/fixture"),
        scope: "fixture".into(),
        servers: vec![ToolServer::Stdio(StdioToolServer {
            name: "name".into(),
            command: "SECRET\0".into(),
            args: vec![],
            env: BTreeMap::from([("secret".into(), "SECRET".into())]),
        })],
    };
    let error = match config.targets() {
        Ok(_) => panic!("expected rejection"),
        Err(error) => error,
    };
    assert_eq!(error, "invalid external tool configuration");
}

fn http_config() -> Configuration {
    Configuration {
        servers: vec![ToolServer::Http(HttpToolServer {
            name: "fixture".into(),
            url: "http://127.0.0.1:1/mcp?opaque=secret".into(),
            headers: BTreeMap::from([("Authorization".into(), "secret".into())]),
        })],
        cwd: std::env::current_dir().unwrap(),
        scope: "scope".into(),
    }
}

#[test]
fn http_header_canonicalization_validates_before_folding_and_preserves_values() {
    let mut original = http_config();
    let mut lower = original.clone();
    if let ToolServer::Http(server) = &mut lower.servers[0] {
        server.headers = BTreeMap::from([("authorization".into(), "secret".into())]);
    }
    original.canonicalize().unwrap();
    lower.canonicalize().unwrap();
    assert!(original == lower);
    if let ToolServer::Http(server) = &mut lower.servers[0] {
        server
            .headers
            .insert("Authorization".into(), "other-secret".into());
    }
    assert_eq!(
        lower.canonicalize().unwrap_err(),
        "invalid external tool configuration"
    );
}

#[test]
fn http_policy_and_shared_configuration_bounds_are_value_free() {
    let mut invalid = vec![];
    for url in [
        "http://user:secret@127.0.0.1/mcp",
        "http://127.0.0.1/mcp#secret",
        "file:///secret",
    ] {
        let mut config = http_config();
        if let ToolServer::Http(server) = &mut config.servers[0] {
            server.url = url.into();
        }
        invalid.push(config);
    }
    for (name, value) in [
        ("Mcp-Session-Id", "secret"),
        ("Host", "secret"),
        ("X-Auth", "secret\r\nheader"),
    ] {
        let mut config = http_config();
        if let ToolServer::Http(server) = &mut config.servers[0] {
            server.headers = BTreeMap::from([(name.into(), value.into())]);
        }
        invalid.push(config);
    }
    let mut mixed = http_config();
    mixed.servers.push(ToolServer::Stdio(StdioToolServer {
        name: "fixture".into(),
        command: "not-spawned".into(),
        args: vec![],
        env: BTreeMap::new(),
    }));
    invalid.push(mixed);
    let mut too_many = http_config();
    too_many.servers = (0..17)
        .map(|i| {
            let mut server = http_config().servers.remove(0);
            if let ToolServer::Http(s) = &mut server {
                s.name = format!("server-{i}");
            }
            server
        })
        .collect();
    invalid.push(too_many);
    for mut config in invalid {
        assert_eq!(
            config.canonicalize().unwrap_err(),
            "invalid external tool configuration"
        );
    }
    let mut oversized = http_config();
    if let ToolServer::Http(server) = &mut oversized.servers[0] {
        server
            .headers
            .insert("X-Secret".into(), "s".repeat(MAX_CATALOG));
    }
    assert_eq!(
        oversized.canonicalize().unwrap_err(),
        "external tool configuration is too large"
    );
}

#[tokio::test]
async fn dropping_discovery_cancels_and_retains_blocking_worker_cleanup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let control = Control::default();
    let worker_control = control.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let worker_finished = finished.clone();
    let task = tokio::task::spawn_blocking(move || {
        while worker_control.check().is_ok() {
            std::thread::sleep(Duration::from_millis(1));
        }
        worker_finished.store(true, Ordering::Release);
        Err("cancelled".into())
    });
    let candidate = Candidate {
        runtime: Some(Arc::new(McpRuntime::new())),
        control: control.clone(),
        task: Some(task),
    };
    drop(candidate);
    assert!(control.check().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while !finished.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
