//! Authenticated ephemeral HTTP boundary tests; no native host or real history.
use rpi_extcore::service::{BackendConfig, Service};
use serde_json::{json, Value};
use std::time::Duration;

#[tokio::test]
async fn child_query_routes_preserve_security_and_strict_bounded_json() {
    let root = std::env::temp_dir().join(format!("rpi-child-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let history = root.join("must-not-read-history");
    let core = Service::new(BackendConfig {
        project: root.clone(),
        registry: root.join("sessions.json"),
        session_dir: Some(history.clone()),
        rpi_bin: root.join("must-not-start.exe"),
        ..Default::default()
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = rpi_httpcore::AppConfig {
        port: address.port(),
        allowed_hosts: Some(vec![address.to_string()]),
        ..Default::default()
    };
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    // This executable contains one test, isolating the temporary environment override.
    let previous = std::env::var_os("RPI_HTTP_PASSWORD");
    let password = uuid::Uuid::new_v4().to_string();
    std::env::set_var("RPI_HTTP_PASSWORD", &password);
    let app = rpi_httpcore::router(core.clone(), &config, receiver.clone());
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
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let base = format!("http://{address}");
    let id = uuid::Uuid::new_v4().to_string();
    let child = format!("tpchild-{}", uuid::Uuid::new_v4());
    let phase = format!("tpphase-{}", uuid::Uuid::new_v4());
    let page = json!({"direction":"newestFirst","limit":8,"maxBytes":32768});
    let queries = [
        ("children/query", page.clone()),
        (
            "phases/query",
            json!({"childId":child,"direction":"newestFirst","limit":8,"maxBytes":32768}),
        ),
        (
            "body/query",
            json!({"childId":child,"phaseEntryId":phase,"descriptorId":"descriptor",
                "limit":2,"maxBytes":65536}),
        ),
        ("snapshot", json!({"children":page,"phaseLimit":2})),
    ];
    for (route, body) in &queries {
        let url = format!("{base}/api/v1/sessions/{id}/tool-program/{route}");
        assert_eq!(
            client
                .post(url)
                .header("X-Rpi-Web", "1")
                .json(body)
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "{route}"
        );
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
    for (route, body) in &queries {
        let url = format!("{base}/api/v1/sessions/{id}/tool-program/{route}");
        assert_eq!(
            client
                .post(&url)
                .header("cookie", &cookie)
                .json(body)
                .send()
                .await
                .unwrap()
                .status(),
            403,
            "missing marker: {route}"
        );
        for (header, value) in [
            ("Host", "untrusted.invalid"),
            ("Origin", "https://untrusted.invalid"),
        ] {
            assert_eq!(
                client
                    .post(&url)
                    .header("cookie", &cookie)
                    .header("X-Rpi-Web", "1")
                    .header(header, value)
                    .json(body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                403,
                "{route}: {header}"
            );
        }
        let mut unknown = body.clone();
        unknown["unknown"] = json!("do-not-echo");
        let mut nested = body.clone();
        if *route == "snapshot" {
            nested["children"]["unknown"] = json!(true);
        } else {
            nested["cursor"] = json!({"unknown":true});
        }
        let mut wrong_type = body.clone();
        if *route == "snapshot" {
            wrong_type["children"]["limit"] = json!("do-not-echo");
        } else {
            wrong_type["limit"] = json!("do-not-echo");
        }
        let depth = format!("{{\"cursor\":{}0{}}}", "[".repeat(160), "]".repeat(160));
        for invalid in [
            String::new(),
            "{".into(),
            "null".into(),
            "[]".into(),
            "{}".into(),
            unknown.to_string(),
            nested.to_string(),
            wrong_type.to_string(),
            depth,
        ] {
            let response = client
                .post(&url)
                .header("cookie", &cookie)
                .header("X-Rpi-Web", "1")
                .header("content-type", "application/json")
                .body(invalid.clone())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 400, "{route}: {invalid}");
            assert_eq!(
                response.json::<Value>().await.unwrap(),
                json!({"error":"invalid JSON request body"})
            );
        }
        // Content-type rejection is sanitized like all other JsonRejections.
        let response = client
            .post(&url)
            .header("cookie", &cookie)
            .header("X-Rpi-Web", "1")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{route}");
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"error":"invalid JSON request body"})
        );

        // Core, not HTTP, validates semantic bounds before looking up the session.
        let mut invalid = body.clone();
        if *route == "snapshot" {
            invalid["phaseLimit"] = json!(0);
        } else {
            invalid["limit"] = json!(0);
        }
        assert_eq!(
            client
                .post(&url)
                .header("cookie", &cookie)
                .header("X-Rpi-Web", "1")
                .json(&invalid)
                .send()
                .await
                .unwrap()
                .status(),
            400,
            "{route}"
        );
        assert_eq!(
            client
                .post(&url)
                .header("cookie", &cookie)
                .header("X-Rpi-Web", "1")
                .json(body)
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "{route}"
        );

        // Retain Axum's existing 2 MiB extractor limit; do not relax it per route.
        // An extractor limit rejection is deliberately a sanitized 400, whereas
        // a core LimitExceeded outcome maps to 413 (covered by server unit tests).
        let mut padded = body.to_string();
        padded.push_str(&" ".repeat(2 * 1024 * 1024 - padded.len()));
        assert_eq!(
            client
                .post(&url)
                .header("cookie", &cookie)
                .header("X-Rpi-Web", "1")
                .header("content-type", "application/json")
                .body(padded.clone())
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "at body limit: {route}"
        );
        padded.push(' ');
        let response = client
            .post(&url)
            .header("cookie", &cookie)
            .header("X-Rpi-Web", "1")
            .header("content-type", "application/json")
            .body(padded)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "over body limit: {route}");
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"error":"invalid JSON request body"})
        );
    }
    // Strict nested continuation DTOs can deserialize and reach core admission.
    let cursor = json!({"sessionId":id,"lane":"main","leafId":"leaf",
        "direction":"newestFirst","childId":null,"afterEntryId":child,"afterSeq":1});
    let mut summary = queries[0].1.clone();
    summary["cursor"] = cursor.clone();
    let mut phases = queries[1].1.clone();
    phases["cursor"] = cursor;
    phases["cursor"]["childId"] = json!(child);
    phases["cursor"]["afterEntryId"] = json!(phase);
    let mut body = queries[2].1.clone();
    body["cursor"] = json!({"direction":"oldestFirst","childId":child,"phaseEntryId":phase,
        "body":{"sessionId":id,"lane":"main","leafId":"leaf",
            "descriptorId":"descriptor","nextIndex":1}});
    for (route, query) in [
        ("children/query", summary),
        ("phases/query", phases),
        ("body/query", body),
    ] {
        assert_eq!(
            client
                .post(format!("{base}/api/v1/sessions/{id}/tool-program/{route}"))
                .header("cookie", &cookie)
                .header("X-Rpi-Web", "1")
                .json(&query)
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "continuation: {route}"
        );
    }
    // Child SSE is a separate authenticated, fresh-snapshot endpoint. It does
    // not reinterpret ordinary transcript Last-Event-ID or accept stale cursors.
    let events = format!("{base}/api/v1/sessions/{id}/tool-program/events");
    assert_eq!(client.get(&events).send().await.unwrap().status(), 401);
    for (header, value) in [
        ("Host", "untrusted.invalid"),
        ("Origin", "https://untrusted.invalid"),
    ] {
        assert_eq!(
            client
                .get(&events)
                .header("cookie", &cookie)
                .header(header, value)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    assert_eq!(
        client
            .get(&events)
            .header("cookie", &cookie)
            .header("Last-Event-ID", "99")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(format!("{events}?after=99"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    shutdown.send(true).unwrap();
    server.await.unwrap();
    core.shutdown().await;
    drop(core);
    assert!(!history.exists());
    assert!(!root.join("must-not-start.exe").exists());
    std::fs::remove_dir_all(root).unwrap();
}
