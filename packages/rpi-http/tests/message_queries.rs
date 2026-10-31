//! Ephemeral authenticated HTTP listener; never starts native or reads real history.
use rpi_extcore::service::{BackendConfig, Service};
use serde_json::json;
use std::time::Duration;
#[tokio::test]
async fn message_query_routes_preserve_security_and_reject_unbounded_or_invalid_input() {
    let root = std::env::temp_dir().join(format!("rpi-message-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let core = Service::new(BackendConfig {
        project: root.clone(),
        registry: root.join("sessions.json"),
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
    // A single test in this executable keeps the temporary env override isolated.
    let previous = std::env::var_os("RPI_HTTP_PASSWORD");
    let password = uuid::Uuid::new_v4().to_string();
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
    let summary = format!("{base}/api/v1/sessions/missing/message-summaries");
    let window = format!("{base}/api/v1/sessions/missing/message-window");
    for url in [&summary, &window] {
        assert_eq!(client.get(url).send().await.unwrap().status(), 401);
    }
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
    for url in [&summary, &window] {
        for (header, value) in [
            ("Host", "untrusted.invalid"),
            ("Origin", "https://untrusted.invalid"),
        ] {
            assert_eq!(
                client
                    .get(url)
                    .header("cookie", &cookie)
                    .header(header, value)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                403
            );
        }
    }
    for query in [
        "limit=0",
        "limit=101",
        "limit=-1",
        "limit=invalid",
        "role=assistant",
        "beforeId=",
        "unknown=true",
    ] {
        assert_eq!(
            client
                .get(format!("{summary}?{query}"))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            400,
            "{query}"
        );
    }
    for query in [
        "",
        "anchorId=a&beforeId=b",
        "anchorId=a&limit=101",
        "afterId=",
        "anchorID=typo",
    ] {
        assert_eq!(
            client
                .get(format!("{window}?{query}"))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            400,
            "{query}"
        );
    }
    for url in [
        format!("{summary}?limit=10&role=user&beforeId=user&leafId=leaf"),
        format!("{window}?afterId=entry&leafId=leaf&limit=48"),
    ] {
        assert_eq!(
            client
                .get(url)
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    shutdown.send(true).unwrap();
    server.await.unwrap();
    core.shutdown().await;
    drop(core);
    std::fs::remove_dir_all(root).unwrap();
}
