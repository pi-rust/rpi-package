//! Isolated HTTP fixture: no native host, deployment registry, or model backend.
use rpi_extcore::service::{BackendConfig, Service};
use serde_json::{json, Value};
use std::{io::Write, time::Duration};

#[tokio::test]
async fn run_timings_preserve_security_validation_and_paging() {
    // One test in this executable, so password environment changes cannot race.
    let previous = std::env::var_os("RPI_HTTP_PASSWORD");
    let password = uuid::Uuid::new_v4().to_string();
    let root = std::env::temp_dir().join(format!("rpi-http-timings-{}", uuid::Uuid::new_v4()));
    let project = root.join("workspace");
    let sessions = root.join("history");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&sessions).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let registry = root.join("sessions.json");
    std::fs::write(
        &registry,
        json!({"sessions":[{"sessionId":id,"createdAt":0,"updatedAt":0}]}).to_string(),
    )
    .unwrap();
    let registry_before = std::fs::read(&registry).unwrap();
    let transcript = sessions.join(format!("2026_{id}.jsonl"));
    let mut file = std::fs::File::create(&transcript).unwrap();
    writeln!(file, "{}", json!({"kind":"header","version":4,"id":id})).unwrap();
    // Exceed the core's bounded record page before the operation, exercising continuation.
    for _ in 0..4200 {
        writeln!(
            file,
            "{}",
            json!({"kind":"record","type":"fixture_padding"})
        )
        .unwrap();
    }
    for row in [
        json!({"kind":"record","type":"operation_started","id":"run","timestamp":1234,"lane":"main","intent":{"kind":"run"}}),
        json!({"kind":"entry","type":"message","id":"actual","lane":"main","message":{"role":"assistant","content":[{"type":"text","text":"fixture"}]}}),
        json!({"kind":"record","type":"operation_finished","id":"finish","runId":"run","timestamp":7890,"lane":"main","outcome":"completed"}),
    ] {
        writeln!(file, "{row}").unwrap();
    }
    drop(file);
    let total_bytes = std::fs::metadata(&transcript).unwrap().len();
    let core = Service::new(BackendConfig {
        project,
        registry: registry.clone(),
        session_dir: Some(sessions),
        rpi_bin: root.join("must-not-start.exe"),
        ..Default::default()
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = rpi_http::AppConfig {
        port: address.port(),
        allowed_hosts: Some(vec![address.to_string()]),
        ..Default::default()
    };
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    std::env::set_var("RPI_HTTP_PASSWORD", &password);
    let app = rpi_http::router(core.clone(), &config, receiver.clone());
    match previous {
        Some(value) => std::env::set_var("RPI_HTTP_PASSWORD", value),
        None => std::env::remove_var("RPI_HTTP_PASSWORD"),
    }
    let server = tokio::spawn(async move {
        let mut receiver = receiver;
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = receiver.changed().await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let base = format!("http://{address}");
    let url = format!("{base}/api/v1/sessions/{id}/run-timings/query");
    let query = json!({"entryIds":["actual"]});
    let response = client
        .post(&url)
        .header("X-Rpi-Web", "1")
        .json(&query)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let login = client
        .post(format!("{base}/api/v1/auth"))
        .header("X-Rpi-Web", "1")
        .json(&json!({"password":password}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let post = |url: &str| {
        client
            .post(url)
            .header("cookie", &cookie)
            .header("X-Rpi-Web", "1")
    };
    for (header, value) in [
        ("Host", "untrusted.invalid"),
        ("Origin", "https://untrusted.invalid"),
    ] {
        let response = post(&url)
            .header(header, value)
            .json(&query)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403, "{header}");
    }
    let response = client
        .post(&url)
        .header("cookie", &cookie)
        .json(&query)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403, "mutation marker remains required");
    let unknown = format!(
        "{base}/api/v1/sessions/{}/run-timings/query",
        uuid::Uuid::new_v4()
    );
    assert_eq!(
        post(&unknown).json(&query).send().await.unwrap().status(),
        404
    );
    let invalid = format!("{base}/api/v1/sessions/not-a-uuid/run-timings/query");
    assert_eq!(
        post(&invalid).json(&query).send().await.unwrap().status(),
        400
    );
    for body in [
        json!({"entryIds":[""]}),
        json!({"entryIds":["actual","actual"]}),
        json!({"entryIds":(0..401).map(|n| n.to_string()).collect::<Vec<_>>()}),
        json!({"entryIds":["x".repeat(257)]}),
        json!({"cursor":"x".repeat(129)}),
        json!({"entryIds":["actual"],"unknown":true}),
        json!({"entryIds":"wrong-type"}),
    ] {
        let response = post(&url).json(&body).send().await.unwrap();
        assert_eq!(response.status(), 400, "{body}");
        assert!(response.json::<Value>().await.unwrap()["error"].is_string());
    }
    assert_eq!(
        post(&url)
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(post(&url).body("{}").send().await.unwrap().status(), 400);
    let response = post(&url).json(&query).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let first: Value = response.json().await.unwrap();
    assert_eq!(first["complete"], false);
    assert_eq!(first["timings"], json!([]));
    assert!(first["cursor"].is_string());
    assert!(first["scannedBytes"].as_u64().unwrap() > 0);
    assert!(first["scannedBytes"].as_u64().unwrap() < total_bytes);
    assert_eq!(first["totalBytes"], total_bytes);
    let response = post(&url)
        .json(&json!({"entryIds":["actual"],"cursor":first["cursor"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let page: Value = response.json().await.unwrap();
    assert_eq!(
        page,
        json!({
            "timings":[{"runId":"run","lane":"main","finalEntryId":"actual","startedAt":1234,"finishedAt":7890,"elapsedMs":6656,"outcome":"completed"}],
            "complete":true,"cursor":first["cursor"],"scannedBytes":total_bytes,"totalBytes":total_bytes
        })
    );
    // The 400-entry boundary is accepted, not rejected by the adapter.
    let response = post(&url).json(&json!({"entryIds":(0..400).map(|n| n.to_string()).collect::<Vec<_>>(),"cursor":page["cursor"]})).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["timings"],
        json!([])
    );
    let response = post(&url)
        .json(&json!({"cursor":"invalid"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(std::fs::read(&registry).unwrap(), registry_before);
    shutdown.send(true).unwrap();
    server.await.unwrap();
    core.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
}
