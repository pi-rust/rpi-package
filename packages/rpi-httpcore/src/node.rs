//! HTTP serialization adapter for the protocol-neutral node discovery document.
//!
//! This router only exposes metadata. It deliberately does not perform
//! authentication or authorization: callers must merge it into a router whose
//! Host/Origin and access policy matches the deployment. Discovery locates a
//! node; it does not grant access.

use axum::{
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rpi_node_core::{NodeDiscoveryDocument, DISCOVERY_PATH};
use serde_json::json;
use std::sync::Arc;

/// Build the RFC 8615 well-known endpoint for a validated node document.
///
/// The document is validated again at request time so this adapter fails
/// closed even if a caller mutates an object through a future shared state
/// implementation. Invalid metadata is never serialized as a successful
/// discovery response.
pub fn node_discovery_router(document: NodeDiscoveryDocument) -> Router {
    Router::new()
        .route(DISCOVERY_PATH, get(discovery))
        .with_state(Arc::new(document))
}

async fn discovery(State(document): State<Arc<NodeDiscoveryDocument>>) -> Response {
    if document.validate().is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "invalid node discovery document"})),
        )
            .into_response();
    }
    let mut response = Json(document.as_ref()).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use rpi_node_core::{
        CapabilityBinding, NodeIdentity, NodeManifest, NodeRole, NodeTransport, ProtocolBinding,
        PROTOCOL, VERSION,
    };
    use tower::ServiceExt;

    fn document() -> NodeDiscoveryDocument {
        let manifest = NodeManifest {
            extension_id: "fixture.node".into(),
            extension_version: "0.1.0".into(),
            protocol: PROTOCOL.into(),
            protocol_version: VERSION,
            roles: vec![NodeRole::Agent],
            bindings: vec![ProtocolBinding {
                protocol: "a2a".into(),
                version: "1".into(),
                transport: NodeTransport::http("https://node.example.test"),
                descriptor: Some("/.well-known/agent-card.json".into()),
            }],
            capabilities: vec![CapabilityBinding {
                namespace: "agent".into(),
                operation: "task.dispatch".into(),
                protocol: "a2a".into(),
                version: Some("1".into()),
            }],
        };
        NodeDiscoveryDocument::from_manifest(
            &NodeIdentity {
                node_id: "node-1".into(),
                owner_epoch: "epoch-1".into(),
            },
            manifest,
            vec![ProtocolBinding {
                protocol: "a2a".into(),
                version: "1".into(),
                transport: NodeTransport::http("https://node.example.test"),
                descriptor: Some("/.well-known/agent-card.json".into()),
            }],
        )
    }

    #[tokio::test]
    async fn well_known_route_serializes_validated_document() {
        let response = node_discovery_router(document())
            .oneshot(
                Request::builder()
                    .uri(DISCOVERY_PATH)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let decoded: NodeDiscoveryDocument = serde_json::from_slice(&body).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.node_id, "node-1");
    }

    #[tokio::test]
    async fn unrelated_path_is_not_exposed() {
        let response = node_discovery_router(document())
            .oneshot(
                Request::builder()
                    .uri("/.well-known/other")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
