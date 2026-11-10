//! Transport-neutral contracts for `rpi --node`.
//!
//! The host owns node identity and lifecycle. An extension contributes a
//! manifest, protocol bindings, domain capabilities and operations; HTTP,
//! WebSocket, stdio, mDNS and registry adapters are outside this crate.
#![forbid(unsafe_code)]

use std::{fmt, pin::Pin};

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL: &str = "rpi-node";
pub const VERSION: u32 = 1;
pub const DISCOVERY_PATH: &str = "/.well-known/rpi-node";
pub const READY_TYPE: &str = "node_ready";
pub const MAX_READY_BYTES: usize = 64 * 1024;
pub const MAX_DESCRIPTOR_BYTES: usize = 256 * 1024;
pub const MAX_DISCOVERY_OBSERVATIONS: usize = 128;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MAX_ID_BYTES: usize = 256;
pub const MAX_CAPABILITIES: usize = 256;
pub const MAX_BINDINGS: usize = 32;
pub const MAX_TRANSPORTS: usize = 16;

/// A role is a semantic description, not an authorization grant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeRole {
    Workspace,
    Agent,
    ToolServer,
    ResourceServer,
    Coordinator,
}

/// An open protocol identifier. Keeping this as a validated value object
/// avoids turning A2A, MCP, ACP or domain protocols into a closed enum.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProtocolId(String);

impl ProtocolId {
    pub fn new(value: impl Into<String>) -> Result<Self, NodeError> {
        let value = value.into();
        validate_id(&value, "protocol")?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProtocolId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ProtocolId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for ProtocolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeTransportKind {
    Http,
    WebSocket,
    Stdio,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeTransport {
    pub kind: NodeTransportKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subprotocol: Option<String>,
}

impl NodeTransport {
    #[must_use]
    pub const fn stdio() -> Self {
        Self {
            kind: NodeTransportKind::Stdio,
            origin: None,
            path: None,
            subprotocol: None,
        }
    }

    #[must_use]
    pub fn http(origin: impl Into<String>) -> Self {
        Self {
            kind: NodeTransportKind::Http,
            origin: Some(origin.into()),
            path: None,
            subprotocol: None,
        }
    }

    #[must_use]
    pub fn websocket(origin: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            kind: NodeTransportKind::WebSocket,
            origin: Some(origin.into()),
            path: Some(path.into()),
            subprotocol: None,
        }
    }

    pub fn validate(&self) -> Result<(), NodeError> {
        if let Some(origin) = &self.origin {
            validate_id(origin, "transport origin")?;
            if origin.contains('@') || origin.contains('?') || origin.contains('#') {
                return Err(NodeError::new(
                    NodeErrorCode::InvalidInput,
                    "transport origin must not contain credentials or query data",
                ));
            }
        }
        if let Some(path) = &self.path {
            validate_id(path, "transport path")?;
        }
        if let Some(subprotocol) = &self.subprotocol {
            validate_id(subprotocol, "transport subprotocol")?;
        }
        if matches!(
            self.kind,
            NodeTransportKind::Http | NodeTransportKind::WebSocket
        ) && self.origin.is_none()
        {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "network transport is missing its origin",
            ));
        }
        Ok(())
    }
}

/// One external protocol exposed by or discovered on a node. `ProtocolId`
/// remains open so A2A, MCP, ACP and future protocols can be added without
/// changing the core enum.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolBinding {
    pub protocol: ProtocolId,
    pub version: String,
    pub transport: NodeTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<String>,
}

impl ProtocolBinding {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(self.protocol.as_str(), "binding protocol")?;
        validate_id(&self.version, "binding version")?;
        self.transport.validate()?;
        if let Some(descriptor) = &self.descriptor {
            validate_id(descriptor, "binding descriptor")?;
        }
        Ok(())
    }
}

/// A namespaced operation supplied by a binding. The protocol remains part of
/// the identity because A2A tasks, MCP tool calls and ACP prompts are not
/// interchangeable merely because they are all remote operations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityBinding {
    pub namespace: String,
    pub operation: String,
    pub protocol: ProtocolId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl CapabilityBinding {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.namespace, "capability namespace")?;
        validate_id(&self.operation, "capability operation")?;
        validate_id(self.protocol.as_str(), "capability protocol")?;
        if let Some(version) = &self.version {
            validate_id(version, "capability version")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeIdentity {
    pub node_id: String,
    pub owner_epoch: String,
}

impl NodeIdentity {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.node_id, "node_id")?;
        validate_id(&self.owner_epoch, "owner_epoch")
    }

    #[must_use]
    pub fn owns(&self, node_id: &str, owner_epoch: &str) -> bool {
        self.node_id == node_id && self.owner_epoch == owner_epoch
    }

    pub fn validate_operation(&self, operation: &OperationRef) -> Result<(), NodeError> {
        operation.validate()?;
        if self.owns(&operation.node_id, &operation.owner_epoch) {
            Ok(())
        } else {
            Err(NodeError::new(
                NodeErrorCode::Conflict,
                "operation belongs to a different node owner epoch",
            ))
        }
    }

    pub fn validate_cursor(&self, cursor: &EventCursor) -> Result<(), NodeError> {
        cursor.validate()?;
        if self.owns(&cursor.node_id, &cursor.owner_epoch) {
            Ok(())
        } else {
            Err(NodeError::new(
                NodeErrorCode::Conflict,
                "event cursor belongs to a different node owner epoch",
            ))
        }
    }
}

/// Static extension declaration and its observable protocol advertisement.
/// Runtime identity, owner epoch and authorization state deliberately live in
/// separate types and are never implied by this manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeManifest {
    pub extension_id: String,
    pub extension_version: String,
    pub protocol: ProtocolId,
    pub protocol_version: u32,
    #[serde(default)]
    pub roles: Vec<NodeRole>,
    #[serde(default)]
    pub bindings: Vec<ProtocolBinding>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityBinding>,
}

impl NodeManifest {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.extension_id, "extension_id")?;
        validate_id(&self.extension_version, "extension_version")?;
        if self.protocol.as_str() != PROTOCOL || self.protocol_version != VERSION {
            return Err(NodeError::new(
                NodeErrorCode::Unsupported,
                "unsupported node protocol",
            ));
        }
        validate_bindings(&self.bindings)?;
        validate_capabilities(&self.capabilities)?;
        for capability in &self.capabilities {
            if !self
                .bindings
                .iter()
                .any(|binding| binding.protocol == capability.protocol)
            {
                return Err(NodeError::new(
                    NodeErrorCode::InvalidInput,
                    "capability references an undisclosed protocol binding",
                ));
            }
        }
        if self.roles.is_empty() {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "node manifest has no role",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeDiscoveryDocument {
    pub protocol: ProtocolId,
    pub version: u32,
    pub node_id: String,
    pub owner_epoch: String,
    pub extension: NodeManifest,
    pub bindings: Vec<ProtocolBinding>,
    pub capabilities: Vec<CapabilityBinding>,
}

impl NodeDiscoveryDocument {
    #[must_use]
    pub fn from_manifest(
        identity: &NodeIdentity,
        mut extension: NodeManifest,
        bindings: Vec<ProtocolBinding>,
    ) -> Self {
        extension.bindings = bindings.clone();
        Self {
            protocol: PROTOCOL.into(),
            version: VERSION,
            node_id: identity.node_id.clone(),
            owner_epoch: identity.owner_epoch.clone(),
            capabilities: extension.capabilities.clone(),
            extension,
            bindings,
        }
    }

    pub fn validate(&self) -> Result<(), NodeError> {
        if self.protocol.as_str() != PROTOCOL || self.version != VERSION {
            return Err(NodeError::new(
                NodeErrorCode::Unsupported,
                "unsupported node discovery protocol",
            ));
        }
        NodeIdentity {
            node_id: self.node_id.clone(),
            owner_epoch: self.owner_epoch.clone(),
        }
        .validate()?;
        self.extension.validate()?;
        validate_bindings(&self.bindings)?;
        validate_capabilities(&self.capabilities)?;
        if self.bindings != self.extension.bindings
            || self.capabilities != self.extension.capabilities
        {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "node discovery does not match the extension manifest",
            ));
        }
        Ok(())
    }
}

/// A stdio bootstrap frame. Network transports should expose the same
/// identity and manifest through `NodeDiscoveryDocument`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeReady {
    #[serde(rename = "type")]
    pub message_type: String,
    pub protocol: ProtocolId,
    pub version: u32,
    pub node_id: String,
    pub owner_epoch: String,
    pub extension: NodeManifest,
    pub bindings: Vec<ProtocolBinding>,
    pub capabilities: Vec<CapabilityBinding>,
}

impl NodeReady {
    #[must_use]
    pub fn from_manifest(identity: &NodeIdentity, manifest: NodeManifest) -> Self {
        Self {
            message_type: READY_TYPE.into(),
            protocol: PROTOCOL.into(),
            version: VERSION,
            node_id: identity.node_id.clone(),
            owner_epoch: identity.owner_epoch.clone(),
            bindings: manifest.bindings.clone(),
            capabilities: manifest.capabilities.clone(),
            extension: manifest,
        }
    }

    pub fn validate(&self) -> Result<(), NodeError> {
        if self.message_type != READY_TYPE
            || self.protocol.as_str() != PROTOCOL
            || self.version != VERSION
        {
            return Err(NodeError::new(
                NodeErrorCode::Unsupported,
                "unsupported node ready frame",
            ));
        }
        NodeIdentity {
            node_id: self.node_id.clone(),
            owner_epoch: self.owner_epoch.clone(),
        }
        .validate()?;
        self.extension.validate()?;
        if self.bindings != self.extension.bindings
            || self.capabilities != self.extension.capabilities
        {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "node ready does not match the extension manifest",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiscoverySource {
    WellKnownHttp,
    StdioReady,
    Mdns,
    Registry,
    ProtocolDescriptor,
}

/// A protocol-specific descriptor retained without forcing its schema into
/// the protocol-neutral core. Examples include an A2A Agent Card or MCP
/// server metadata document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalDescriptor {
    pub protocol: ProtocolId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub payload: Value,
}

impl ExternalDescriptor {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(self.protocol.as_str(), "descriptor protocol")?;
        if let Some(version) = &self.version {
            validate_id(version, "descriptor version")?;
        }
        let encoded = serde_json::to_vec(&self.payload).map_err(|_| {
            NodeError::new(
                NodeErrorCode::InvalidInput,
                "external descriptor payload is not serializable",
            )
        })?;
        if encoded.len() > MAX_DESCRIPTOR_BYTES {
            return Err(NodeError::new(
                NodeErrorCode::LimitExceeded,
                "external descriptor payload is too large",
            ));
        }
        Ok(())
    }
}

/// The normalized result of any discovery provider. `external_descriptor` is
/// kept so an adapter does not have to discard protocol-specific metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryObservation {
    pub source: DiscoverySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<NodeTransport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<NodeIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<NodeManifest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_descriptor: Option<ExternalDescriptor>,
}

impl DiscoveryObservation {
    pub fn validate(&self) -> Result<(), NodeError> {
        if let Some(transport) = &self.transport {
            transport.validate()?;
        }
        if let Some(identity) = &self.identity {
            identity.validate()?;
        }
        if let Some(manifest) = &self.manifest {
            manifest.validate()?;
        }
        if let Some(descriptor) = &self.external_descriptor {
            descriptor.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeRequest {
    pub request_id: String,
    pub operation: String,
    #[serde(default)]
    pub payload: Value,
}

impl NodeRequest {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.request_id, "request_id")?;
        validate_id(&self.operation, "operation")?;
        validate_payload(&self.payload, "node request")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeResponse {
    pub request_id: String,
    #[serde(default)]
    pub payload: Value,
}

impl NodeResponse {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.request_id, "request_id")?;
        validate_payload(&self.payload, "node response")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationRef {
    pub node_id: String,
    pub owner_epoch: String,
    pub operation_id: String,
}

impl OperationRef {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.node_id, "node_id")?;
        validate_id(&self.owner_epoch, "owner_epoch")?;
        validate_id(&self.operation_id, "operation_id")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum OperationStatus {
    Reserved,
    Dispatching,
    KnownResult { response: NodeResponse },
    OutcomeUnknown,
    Expired,
}

impl OperationStatus {
    pub fn validate(&self) -> Result<(), NodeError> {
        if let Self::KnownResult { response } = self {
            response.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventCursor {
    pub node_id: String,
    pub owner_epoch: String,
    pub sequence: u64,
}

impl EventCursor {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.node_id, "node_id")?;
        validate_id(&self.owner_epoch, "owner_epoch")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeEvent {
    pub sequence: u64,
    pub event_type: String,
    #[serde(default)]
    pub payload: Value,
}

impl NodeEvent {
    pub fn validate(&self) -> Result<(), NodeError> {
        validate_id(&self.event_type, "event_type")?;
        validate_payload(&self.payload, "node event")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeErrorCode {
    InvalidInput,
    Unsupported,
    NotFound,
    AccessDenied,
    Conflict,
    Unavailable,
    LimitExceeded,
    OutcomeUnknown,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeError {
    pub code: NodeErrorCode,
    pub message: String,
}

impl NodeError {
    #[must_use]
    pub fn new(code: NodeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node error ({:?}): {}", self.code, self.message)
    }
}

impl std::error::Error for NodeError {}

pub type NodeEventStream =
    Pin<Box<dyn Stream<Item = Result<NodeEvent, NodeError>> + Send + 'static>>;

fn validate_bindings(bindings: &[ProtocolBinding]) -> Result<(), NodeError> {
    if bindings.is_empty() || bindings.len() > MAX_BINDINGS {
        return Err(NodeError::new(
            NodeErrorCode::InvalidInput,
            "node manifest has an invalid protocol binding list",
        ));
    }
    for (index, binding) in bindings.iter().enumerate() {
        binding.validate()?;
        if bindings[..index].iter().any(|existing| existing == binding) {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "node manifest contains a duplicate protocol binding",
            ));
        }
    }
    Ok(())
}

fn validate_capabilities(capabilities: &[CapabilityBinding]) -> Result<(), NodeError> {
    if capabilities.len() > MAX_CAPABILITIES {
        return Err(NodeError::new(
            NodeErrorCode::LimitExceeded,
            "node capability list is too large",
        ));
    }
    for (index, capability) in capabilities.iter().enumerate() {
        capability.validate()?;
        if capabilities[..index]
            .iter()
            .any(|existing| existing == capability)
        {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "node manifest contains a duplicate capability",
            ));
        }
    }
    Ok(())
}

fn validate_payload(value: &Value, field: &str) -> Result<(), NodeError> {
    let encoded = serde_json::to_vec(value).map_err(|_| {
        NodeError::new(
            NodeErrorCode::InvalidInput,
            format!("{field} payload is not serializable"),
        )
    })?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(NodeError::new(
            NodeErrorCode::LimitExceeded,
            format!("{field} payload is too large"),
        ));
    }
    Ok(())
}

fn validate_id(value: &str, field: &str) -> Result<(), NodeError> {
    if value.trim().is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control)
    {
        return Err(NodeError::new(
            NodeErrorCode::InvalidInput,
            format!("invalid {field}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> NodeManifest {
        NodeManifest {
            extension_id: "rpi.work".into(),
            extension_version: "0.1.0".into(),
            protocol: PROTOCOL.into(),
            protocol_version: VERSION,
            roles: vec![NodeRole::Workspace, NodeRole::Agent],
            bindings: vec![
                ProtocolBinding {
                    protocol: "rpi-node".into(),
                    version: "1".into(),
                    transport: NodeTransport::stdio(),
                    descriptor: None,
                },
                ProtocolBinding {
                    protocol: "a2a".into(),
                    version: "1".into(),
                    transport: NodeTransport::http("https://node.example.test"),
                    descriptor: Some("/.well-known/agent-card.json".into()),
                },
                ProtocolBinding {
                    protocol: "rpi-work-node".into(),
                    version: "1".into(),
                    transport: NodeTransport::websocket(
                        "https://node.example.test",
                        "/api/v1/realtime",
                    ),
                    descriptor: None,
                },
                ProtocolBinding {
                    protocol: "mcp".into(),
                    version: "2025-06-18".into(),
                    transport: NodeTransport::stdio(),
                    descriptor: None,
                },
                ProtocolBinding {
                    protocol: "acp".into(),
                    version: "1".into(),
                    transport: NodeTransport::stdio(),
                    descriptor: None,
                },
            ],
            capabilities: vec![
                CapabilityBinding {
                    namespace: "sessions".into(),
                    operation: "prompt".into(),
                    protocol: "rpi-work-node".into(),
                    version: Some("1".into()),
                },
                CapabilityBinding {
                    namespace: "agent".into(),
                    operation: "task.dispatch".into(),
                    protocol: "a2a".into(),
                    version: Some("1".into()),
                },
                CapabilityBinding {
                    namespace: "tools".into(),
                    operation: "call".into(),
                    protocol: "mcp".into(),
                    version: Some("2025-06-18".into()),
                },
                CapabilityBinding {
                    namespace: "session".into(),
                    operation: "prompt".into(),
                    protocol: "acp".into(),
                    version: Some("1".into()),
                },
            ],
        }
    }

    fn identity() -> NodeIdentity {
        NodeIdentity {
            node_id: "node-1".into(),
            owner_epoch: "epoch-1".into(),
        }
    }

    #[test]
    fn manifest_rejects_duplicate_bindings_and_capabilities() {
        let mut duplicate_binding = manifest();
        let binding = duplicate_binding.bindings[0].clone();
        duplicate_binding.bindings.push(binding);
        assert_eq!(
            duplicate_binding.validate().unwrap_err().code,
            NodeErrorCode::InvalidInput
        );

        let mut duplicate_capability = manifest();
        let capability = duplicate_capability.capabilities[0].clone();
        duplicate_capability.capabilities.push(capability);
        assert_eq!(
            duplicate_capability.validate().unwrap_err().code,
            NodeErrorCode::InvalidInput
        );
    }

    #[test]
    fn transport_rejects_embedded_credentials() {
        let transport = NodeTransport::http("https://user:secret@node.example.test");
        assert_eq!(
            transport.validate().unwrap_err().code,
            NodeErrorCode::InvalidInput
        );
    }

    #[test]
    fn ready_round_trips_and_validates() {
        let ready = NodeReady::from_manifest(&identity(), manifest());
        ready.validate().unwrap();
        let decoded: NodeReady =
            serde_json::from_str(&serde_json::to_string(&ready).unwrap()).unwrap();
        assert_eq!(decoded, ready);
    }

    #[test]
    fn ready_rejects_capability_drift() {
        let mut ready = NodeReady::from_manifest(&identity(), manifest());
        ready.capabilities.pop();
        assert_eq!(
            ready.validate().unwrap_err().code,
            NodeErrorCode::InvalidInput
        );
    }

    #[test]
    fn discovery_preserves_foreign_protocol_bindings() {
        let manifest = manifest();
        let document = NodeDiscoveryDocument {
            protocol: PROTOCOL.into(),
            version: VERSION,
            node_id: identity().node_id,
            owner_epoch: identity().owner_epoch,
            bindings: manifest.bindings.clone(),
            capabilities: manifest.capabilities.clone(),
            extension: manifest,
        };
        document.validate().unwrap();
        assert_eq!(document.bindings[1].protocol.as_str(), "a2a");
        assert_eq!(
            document.bindings[1].descriptor.as_deref(),
            Some("/.well-known/agent-card.json")
        );
        assert!(document
            .bindings
            .iter()
            .any(|binding| binding.protocol.as_str() == "mcp"));
        assert!(document
            .capabilities
            .iter()
            .any(|capability| capability.protocol.as_str() == "acp"));
    }

    #[test]
    fn observation_keeps_raw_external_descriptor() {
        let observation = DiscoveryObservation {
            source: DiscoverySource::ProtocolDescriptor,
            transport: Some(NodeTransport::http("https://node.example.test")),
            identity: None,
            manifest: None,
            external_descriptor: Some(ExternalDescriptor {
                protocol: "a2a".into(),
                version: Some("1".into()),
                payload: serde_json::json!({"protocolVersion":"a2a-v1"}),
            }),
        };
        observation.validate().unwrap();
        assert!(observation.external_descriptor.is_some());
    }

    #[test]
    fn external_descriptor_is_bounded() {
        let descriptor = ExternalDescriptor {
            protocol: "mcp".into(),
            version: Some("1".into()),
            payload: serde_json::json!({
                "metadata": "x".repeat(MAX_DESCRIPTOR_BYTES),
            }),
        };
        assert_eq!(
            descriptor.validate().unwrap_err().code,
            NodeErrorCode::LimitExceeded
        );
    }

    #[test]
    fn operation_status_validates_known_results() {
        let status = OperationStatus::KnownResult {
            response: NodeResponse {
                request_id: String::new(),
                payload: Value::Null,
            },
        };
        assert_eq!(
            status.validate().unwrap_err().code,
            NodeErrorCode::InvalidInput
        );
    }

    #[test]
    fn event_payloads_are_bounded() {
        let event = NodeEvent {
            sequence: 1,
            event_type: "large".into(),
            payload: serde_json::json!({"data": "x".repeat(MAX_MESSAGE_BYTES)}),
        };
        assert_eq!(
            event.validate().unwrap_err().code,
            NodeErrorCode::LimitExceeded
        );
    }

    #[test]
    fn request_and_response_payloads_are_bounded() {
        let request = NodeRequest {
            request_id: "request-1".into(),
            operation: "large".into(),
            payload: serde_json::json!({"data": "x".repeat(MAX_MESSAGE_BYTES)}),
        };
        assert_eq!(
            request.validate().unwrap_err().code,
            NodeErrorCode::LimitExceeded
        );

        let response = NodeResponse {
            request_id: "request-1".into(),
            payload: serde_json::json!({"data": "x".repeat(MAX_MESSAGE_BYTES)}),
        };
        assert_eq!(
            response.validate().unwrap_err().code,
            NodeErrorCode::LimitExceeded
        );
    }

    #[test]
    fn stale_operation_and_cursor_are_rejected_after_owner_change() {
        let current = identity();
        let operation = OperationRef {
            node_id: "node-1".into(),
            owner_epoch: "epoch-0".into(),
            operation_id: "operation-1".into(),
        };
        let cursor = EventCursor {
            node_id: "node-1".into(),
            owner_epoch: "epoch-0".into(),
            sequence: 4,
        };
        assert_eq!(
            current.validate_operation(&operation).unwrap_err().code,
            NodeErrorCode::Conflict
        );
        assert_eq!(
            current.validate_cursor(&cursor).unwrap_err().code,
            NodeErrorCode::Conflict
        );
        assert!(!current.owns("node-1", "epoch-0"));
    }
}
