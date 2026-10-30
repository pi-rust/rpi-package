//! Isolated loopback integration fixture; never launches a native host or persistent service.
use rpi_extcore::service::{self, BackendConfig, Service};
use serde_json::{json, Value};
use std::time::Duration;

const PNG: &[u8] = include_bytes!("../../rpi-extcore/tests/fixtures/project-image.png");

#[tokio::test]
async fn project_images_preserve_auth_scope_and_binary_responses() {
    // This integration-test executable has one test, so environment changes cannot race
    // other tests. Never log or use the deployment's password.
    let previous = std::env::var_os("RPI_HTTP_PASSWORD");
    let password = uuid::Uuid::new_v4().to_string();
    std::env::set_var("RPI_HTTP_PASSWORD", &password);
    let root = std::env::temp_dir().join(format!("rpi-images-{}", uuid::Uuid::new_v4()));
    let project = root.join("workspace");
    std::fs::create_dir_all(project.join("artifacts")).unwrap();
    std::fs::create_dir_all(project.join(".deploy")).unwrap();
    std::fs::create_dir_all(project.join("folder.png")).unwrap();
    std::fs::write(project.join("artifacts/theme-design-mobile-390.png"), PNG).unwrap();
    std::fs::write(project.join("fake.png"), b"<html>not an image</html>").unwrap();
    std::fs::write(project.join("fake.svg"), b"<svg/>").unwrap();
    std::fs::write(project.join(".deploy/secret.png"), PNG).unwrap();
    let large = std::fs::File::create(project.join("large.png")).unwrap();
    large.set_len(20 * 1024 * 1024 + 1).unwrap();
    drop(large);
    #[cfg(unix)]
    {
        std::fs::write(root.join("outside.png"), PNG).unwrap();
        std::os::unix::fs::symlink(root.join("outside.png"), project.join("escape.png")).unwrap();
        std::os::unix::fs::symlink(project.join(".deploy"), project.join("alias")).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(project.join("pipe.png"))
            .status()
            .unwrap()
            .success());
    }
    let core = Service::new(BackendConfig {
        project: project.clone(),
        registry: root.join("sessions.json"),
        ..Default::default()
    })
    .unwrap();
    let projects = service::list_projects(&core).await;
    let id = projects["projects"][0]["id"].as_str().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = rpi_http::AppConfig {
        port: address.port(),
        allowed_hosts: Some(vec![address.to_string()]),
        ..Default::default()
    };
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
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
    let image_url = format!("{base}/api/v1/projects/{id}/image");
    let response = client
        .get(&image_url)
        .query(&[("path", "artifacts/theme-design-mobile-390.png")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
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
    let response = client
        .get(&image_url)
        .header("cookie", &cookie)
        .query(&[("path", "artifacts/theme-design-mobile-390.png")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.bytes().await.unwrap().as_ref(), PNG);
    for (header, value) in [
        ("Host", "untrusted.invalid"),
        ("Origin", "https://untrusted.invalid"),
    ] {
        let response = client
            .get(&image_url)
            .header("cookie", &cookie)
            .header(header, value)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
        assert_eq!(response.headers()["cache-control"], "private, no-store");
    }
    let response = client
        .get(format!("{base}/api/v1/projects/unknown/image"))
        .header("cookie", &cookie)
        .query(&[("path", "fake.png")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    for (path, status) in [
        ("../outside.png", 403),
        ("/artifacts/theme-design-mobile-390.png", 403),
        (".deploy/secret.png", 403),
        (".rpi/image.png", 403),
        (".git/image.png", 403),
        ("fake.png", 415),
        ("fake.svg", 415),
        ("large.png", 413),
        ("missing.png", 404),
        ("folder.png", 400),
    ] {
        let response = client
            .get(&image_url)
            .header("cookie", &cookie)
            .query(&[("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "{path}");
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        let body: Value = response.json().await.unwrap();
        assert!(body["error"].is_string(), "{path}");
    }
    #[cfg(unix)]
    for (path, status) in [
        ("escape.png", 403),
        ("alias/secret.png", 403),
        ("pipe.png", 400),
    ] {
        let response = client
            .get(&image_url)
            .header("cookie", &cookie)
            .query(&[("path", path)])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "{path}");
        assert_eq!(response.headers()["cache-control"], "private, no-store");
    }
    shutdown.send(true).unwrap();
    server.await.unwrap();
    core.shutdown().await;

    // The standalone/web listener uses serve_until, not the embedded router builder.
    // Its outer middleware must apply the same cache policy to failed image reads.
    let (listening, address) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let standalone = tokio::spawn(rpi_http::serve_until(
        rpi_http::AppConfig {
            project,
            registry: root.join("standalone-sessions.json"),
            port: 0,
            ..Default::default()
        },
        axum::Router::new(),
        move |info| {
            let _ = listening.send(info.address);
        },
        async {
            let _ = stopped.await;
        },
    ));
    let address = address.await.unwrap();
    let response = client
        .get(format!(
            "http://{address}/api/v1/projects/unknown/image?path=missing.png"
        ))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    stop.send(()).unwrap();
    standalone.await.unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
