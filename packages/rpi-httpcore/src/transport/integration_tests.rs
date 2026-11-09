use super::{
    auth_router, bearer_auth_router, lifecycle_router, same_origin_router, ConnectionPolicy,
    HostOriginPolicy, TransportState,
};
use crate::access::{AccessState, AuthLease};
use axum::{
    body::Body,
    extract::{ws::WebSocketUpgrade, Extension},
    http::{header, HeaderMap, Method, Request, StatusCode},
    routing::get,
    Router,
};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, protocol::frame::coding::CloseCode, Message},
};
use tower::ServiceExt;

fn request(method: Method, credential: &str, marker: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri("http://127.0.0.1:3030/")
        .header(header::HOST, "127.0.0.1:3030")
        .header(header::ORIGIN, "http://127.0.0.1:3030")
        .header(header::AUTHORIZATION, format!("Bearer {credential}"));
    if marker {
        builder = builder.header("x-rpi-web", "1");
    }
    builder.body(Body::empty()).unwrap()
}

async fn protected_app(access: AccessState, operations: u32) -> (Router, AuthLease) {
    let lease = access
        .issue(
            crate::access::PrincipalId::new("integration").unwrap(),
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let routes = Router::new().route(
        "/",
        get(|| async { StatusCode::OK }).post(|| async { StatusCode::CREATED }),
    );
    let transport = TransportState::new(
        access.clone(),
        operations,
        ConnectionPolicy::new(8, 1024, 4).unwrap(),
    )
    .unwrap();
    let app = lifecycle_router(
        same_origin_router(
            bearer_auth_router(routes, access),
            HostOriginPolicy::new(["127.0.0.1:3030".to_owned()]),
        ),
        transport,
    );
    (app, lease)
}

#[tokio::test]
async fn composed_security_layers_validate_host_origin_and_bearer_lease() {
    let access = AccessState::new(true, 4);
    let (app, lease) = protected_app(access, 4).await;

    let unauthorized = app
        .clone()
        .oneshot(request(Method::GET, "missing", false))
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized.headers()["x-content-type-options"], "nosniff");

    let mut cross_origin = request(Method::GET, lease.credential(), false);
    cross_origin
        .headers_mut()
        .insert(header::ORIGIN, "http://evil.invalid".parse().unwrap());
    let forbidden = app.clone().oneshot(cross_origin).await.unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let ok = app
        .oneshot(request(Method::GET, lease.credential(), false))
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn custom_authenticator_can_be_composed_with_owner_router() {
    let access = AccessState::new(true, 4);
    let lease = access
        .issue(
            crate::access::PrincipalId::new("custom-auth").unwrap(),
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let expected = lease.credential().to_owned();
    let routes = Router::new().route("/", get(|| async { StatusCode::OK }));
    let app = auth_router(
        routes,
        access,
        move |access: AccessState, headers: HeaderMap| {
            let expected = expected.clone();
            async move {
                if headers
                    .get("x-custom-auth")
                    .and_then(|value| value.to_str().ok())
                    != Some(expected.as_str())
                {
                    return None;
                }
                access.authenticate(&expected).await.ok()
            }
        },
    );
    let mut request = request(Method::GET, "unused", false);
    request
        .headers_mut()
        .insert("x-custom-auth", lease.credential().parse().unwrap());
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn composed_security_layers_require_mutation_marker() {
    let access = AccessState::new(true, 4);
    let (app, lease) = protected_app(access, 4).await;

    let missing = app
        .clone()
        .oneshot(request(Method::POST, lease.credential(), false))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let accepted = app
        .oneshot(request(Method::POST, lease.credential(), true))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn websocket_upgrade_binds_lease_and_closes_on_revoke() {
    let access = AccessState::new(true, 4);
    let lease = access
        .issue(
            crate::access::PrincipalId::new("websocket").unwrap(),
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let registry = super::ConnectionRegistry::new(
        access.clone(),
        super::ConnectionPolicy::new(1, 1024, 4).unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = same_origin_router(
        bearer_auth_router(
            Router::new()
                .route(
                    "/",
                    get(|
                        upgrade: WebSocketUpgrade,
                        Extension(lease): Extension<crate::access::AuthLease>,
                        Extension(registry): Extension<super::ConnectionRegistry>,
                    | async move {
                        super::upgrade_websocket(
                            upgrade,
                            registry,
                            lease,
                            |mut session| async move {
                                let _ = session.recv().await;
                            },
                        )
                        .await
                    }),
                )
                .layer(Extension(registry.clone())),
            access.clone(),
        ),
        HostOriginPolicy::new([address.to_string()]),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut request = format!("ws://{address}/").into_client_request().unwrap();
    request
        .headers_mut()
        .insert(header::HOST, address.to_string().parse().unwrap());
    request
        .headers_mut()
        .insert(header::ORIGIN, format!("http://{address}").parse().unwrap());
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {}", lease.credential()).parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    access.revoke(&lease).await;
    let message = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        futures_util::StreamExt::next(&mut socket),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(matches!(message, Message::Close(Some(frame)) if frame.code == CloseCode::Policy));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn lifecycle_layer_rejects_at_capacity_and_drains() {
    let access = AccessState::new(true, 4);
    let lease = access
        .issue(
            crate::access::PrincipalId::new("admission").unwrap(),
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let started_handler = started.clone();
    let release_handler = release.clone();
    let routes = Router::new().route(
        "/",
        get(move || {
            let started = started_handler.clone();
            let release = release_handler.clone();
            async move {
                started.notify_one();
                release.notified().await;
                StatusCode::OK
            }
        }),
    );
    let transport =
        TransportState::new(access, 1, ConnectionPolicy::new(8, 1024, 4).unwrap()).unwrap();
    let app = lifecycle_router(routes, transport);
    let first = {
        let app = app.clone();
        let credential = lease.credential().to_owned();
        tokio::spawn(async move {
            app.oneshot(request(Method::GET, &credential, false))
                .await
                .unwrap()
        })
    };
    started.notified().await;
    let saturated = app
        .oneshot(request(Method::GET, lease.credential(), false))
        .await
        .unwrap();
    assert_eq!(saturated.status(), StatusCode::TOO_MANY_REQUESTS);
    release.notify_one();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}
