//! MCP2026 ACP transport mapping. Registrations belong to the Writer connection;
//! each core binding owns only its operations and keeps a weak Writer reference.
use crate::protocol::Writer;
use rpi_extcore::mcp_external::{Backend, BackendFuture, CancellationToken, ExternalConfig, Operation, Outcome};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::{Arc, Weak}};

pub fn advertise(result: &mut Value, enabled: bool, v2: bool) {
    if !enabled { return; }
    if v2 { result["capabilities"]["session"]["mcp"]["acp"] = json!({}); }
    else { result["agentCapabilities"]["mcpCapabilities"]["acp"] = json!(true); }
}

pub(crate) fn register(writer: &Writer, declarations: &BTreeMap<String, String>) -> Result<(), String> {
    let mut registrations = writer.mcp_registrations.lock().map_err(|_| "ACP MCP registration lock failed")?;
    if declarations.iter().any(|(id, name)| registrations.get(id).is_some_and(|old| old != name)) {
        return Err("ACP MCP serverId cannot be rebound".into());
    }
    registrations.extend(declarations.clone());
    Ok(())
}

/// The carrier is NOT an outer JSON-RPC response. Explicit null is a result,
/// result wins over error, and all inner extensions/data stay opaque.
pub fn outcome(carrier: Value) -> Result<Outcome, String> {
    let carrier = carrier.as_object().ok_or("invalid ACP MCP outcome carrier")?;
    if let Some(result) = carrier.get("result") { return Ok(Outcome::Result(result.clone())); }
    let error = carrier.get("error").filter(|v| v.is_object()).ok_or("missing ACP MCP outcome")?;
    serde_json::from_value(error.clone()).map(Outcome::Error).map_err(|_| "invalid ACP MCP protocol error".into())
}

pub struct AcpBackend {
    writer: Weak<Writer>,
    closing: CancellationToken,
    active: tokio::sync::RwLock<()>,
}
impl AcpBackend {
    pub fn new(writer: &Arc<Writer>) -> Self {
        Self { writer: Arc::downgrade(writer), closing: CancellationToken::new(), active: Default::default() }
    }
}
impl Backend for AcpBackend {
    fn request(&self, operation: Operation, cancellation: CancellationToken) -> BackendFuture<'_, Result<Outcome, String>> {
        Box::pin(async move {
            let _active = self.active.read().await;
            if self.closing.is_cancelled() { return Err("ACP MCP binding closed".into()); }
            let writer = self.writer.upgrade().ok_or("ACP MCP connection closed")?;
            let cancel = self.closing.child_token();
            let request = writer.request_checked("mcp/message", json!({
                "serverId":operation.server_id,"requestId":operation.request_id,
                "method":operation.method,"params":operation.params
            }), cancel.clone());
            tokio::pin!(request);
            let carrier = tokio::select! {
                result = &mut request => result,
                _ = cancellation.cancelled() => { cancel.cancel(); request.await },
            }?;
            outcome(carrier)
        })
    }
    fn shutdown(&self) -> BackendFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.closing.cancel();
            let _joined = self.active.write().await;
            Ok(())
        })
    }
}

pub(crate) fn external(writer: &Arc<Writer>, servers: BTreeMap<String, String>) -> ExternalConfig {
    ExternalConfig {
        servers,
        metadata: BTreeMap::from([
            ("io.modelcontextprotocol/protocolVersion".into(), json!(rpi_extcore::mcp_external::PROTOCOL_VERSION)),
            ("io.modelcontextprotocol/clientCapabilities".into(), json!({})),
            ("io.modelcontextprotocol/clientInfo".into(), json!({"name":"rpi-acp","version":env!("CARGO_PKG_VERSION")})),
        ]),
        backend: Arc::new(AcpBackend::new(writer)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carrier_preserves_domains_null_and_extensions() {
        assert_eq!(outcome(json!({"result":null,"error":{"code":1,"message":"ignored"},"_meta":false})).unwrap(), Outcome::Result(Value::Null));
        let error = json!({"code":-32000,"message":"inner","data":null,"future":{"opaque":true}});
        let Outcome::Error(parsed) = outcome(json!({"error":error,"unknown":1})).unwrap() else { panic!() };
        assert_eq!(serde_json::to_value(parsed).unwrap(), error);
        assert!(outcome(json!({"error":null})).is_err());
        assert!(outcome(json!({"error":{"code":"wrong","message":"x"}})).is_err());
    }
    #[test]
    fn declarations_opt_in_and_connection_identity() {
        let declaration = json!({"mcpServers":[{"type":"acp","name":"human / tools","serverId":"provider:1"}]});
        assert!(crate::mcp::parse(&declaration, false, false).is_err());
        let a = crate::mcp::parse(&declaration, false, true).unwrap();
        let b = crate::mcp::parse(&declaration, true, true).unwrap();
        assert!(a == b);
        assert!(a.1.keys().all(|key| key.starts_with("acp_") && key.len() == 68));
        let writer = Writer::with_output(tokio::io::sink());
        register(&writer, &a.2).unwrap();
        register(&writer, &b.2).unwrap();
        assert!(register(&writer, &BTreeMap::from([("provider:1".into(), "different".into())])).is_err());
        let mut caps = json!({"capabilities":{"session":{}}});
        advertise(&mut caps, false, true);
        assert!(caps.pointer("/capabilities/session/mcp/acp").is_none());
        advertise(&mut caps, true, true);
        assert_eq!(caps.pointer("/capabilities/session/mcp/acp"), Some(&json!({})));
    }
}
