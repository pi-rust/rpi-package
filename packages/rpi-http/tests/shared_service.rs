//! Rust consumers and HTTP clients must operate on the same service instance.
use rpi_extcore::service::{self, BackendConfig, CreateProjectRequest, Service};
use serde_json::{json, Value};
use std::time::Duration;

#[tokio::test]
async fn http_and_rust_share_project_state_without_frontend_or_plugin_runtime() {
    let root = std::env::temp_dir().join(format!("rpi-http-shared-{}", uuid::Uuid::new_v4()));
    let project = root.join("workspace");
    std::fs::create_dir_all(&project).unwrap();
    let core = Service::new(BackendConfig {
        project: project.clone(),
        registry: root.join("sessions.json"),
        projects_root: Some(root.join("managed")),
        ..BackendConfig::default()
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = rpi_http::AppConfig {
        port: address.port(),
        ..Default::default()
    };
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let app = rpi_http::router(core.clone(), &config, receiver.clone());
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
    let auth_status: Value = client
        .get(format!("{base}/api/v1/auth"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let client = if auth_status["required"] == true {
        let password = std::env::var("RPI_HTTP_PASSWORD")
            .or_else(|_| std::env::var("RPI_WEB_PASSWORD"))
            .expect("an enabled HTTP test server requires its configured password");
        let response = client
            .post(format!("{base}/api/v1/auth"))
            .header("X-Rpi-Web", "1")
            .json(&json!({"password": password}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let cookie = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("HTTP login should return a session cookie");
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::COOKIE,
            reqwest::header::HeaderValue::from_str(cookie).unwrap(),
        );
        reqwest::Client::builder()
            .default_headers(headers)
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    } else {
        client
    };
    let outcome = service::create_project(
        &core,
        CreateProjectRequest {
            name: "from-rust".into(),
            path: None,
            init_git: false,
        },
    )
    .await;
    assert_eq!(outcome.0, rpi_extcore::reply::ReplyKind::Created);
    let projects: Value = client
        .get(format!("{base}/api/v1/projects"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|project| project["name"] == "from-rust"));
    let created = client
        .post(format!("{base}/api/v1/projects"))
        .header("X-Rpi-Web", "1")
        .json(&json!({"name":"from-http"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let projects = service::list_projects(&core).await;
    assert!(projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|project| project["name"] == "from-http"));
    let durable_state = client
        .get(format!("{base}/api/v1/sessions/missing/durable"))
        .send()
        .await
        .unwrap();
    assert_eq!(durable_state.status(), reqwest::StatusCode::NOT_FOUND);
    let durable_state_body: Value = durable_state.json().await.unwrap();
    assert_eq!(durable_state_body["code"], "session_not_found");

    let durable_events = client
        .get(format!("{base}/api/v1/sessions/missing/durable/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(durable_events.status(), reqwest::StatusCode::NOT_FOUND);
    let durable_events_body: Value = durable_events.json().await.unwrap();
    assert_eq!(durable_events_body["code"], "session_not_found");

    for path in ["/", "/assets/index.js"] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/api/v1/projects"))
            .header("X-Rpi-Web", "1")
            .json(&json!({"name":"../outside"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    shutdown.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    core.shutdown().await;
    std::fs::remove_dir_all(&root).unwrap();
}
