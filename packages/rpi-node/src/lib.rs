//! Host-side lifecycle primitives for `rpi --node`.
//!
//! The host owns identity, owner generations and extension startup. Discovery
//! providers such as the mDNS adapter live here as transport-neutral host
//! integrations; HTTP routing remains in `rpi-httpcore`.
#![forbid(unsafe_code)]

mod mdns;

pub use mdns::{
    MdnsAdvertisement, MdnsDiscoveryProvider, DEFAULT_MDNS_DISCOVERY_TIMEOUT, MDNS_SERVICE_TYPE,
};

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use rpi_node_core::{
    CapabilityBinding, DiscoveryObservation, DiscoverySource, EventCursor, NodeDiscoveryDocument,
    NodeError, NodeErrorCode, NodeEventStream, NodeIdentity, NodeManifest, NodeReady, NodeRequest,
    NodeResponse, ProtocolBinding, MAX_DISCOVERY_OBSERVATIONS, MAX_READY_BYTES,
};
use serde::{Deserialize, Serialize};

pub type NodeFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, NodeError>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeHostContext {
    pub identity: NodeIdentity,
    pub extension_id: String,
}

pub trait NodeExtension: Send + Sync {
    fn manifest(&self) -> NodeManifest;

    fn start(&self, context: NodeHostContext) -> NodeFuture<'_, Arc<dyn NodeInstance>>;
}

pub trait NodeInstance: Send + Sync {
    fn capabilities(&self) -> Vec<CapabilityBinding>;

    fn query<'a>(&'a self, request: NodeRequest) -> NodeFuture<'a, NodeResponse>;

    fn mutate<'a>(&'a self, request: NodeRequest) -> NodeFuture<'a, NodeResponse>;

    fn subscribe(&self, after: Option<EventCursor>) -> NodeEventStream;

    fn shutdown<'a>(&'a self) -> NodeFuture<'a, ()>;
}

/// A host-level hook for future A2A, MCP, ACP and other protocol adapters.
/// Adapters are selected by protocol binding, not by a hard-coded node type.
pub trait ProtocolAdapter: Send + Sync {
    fn protocol(&self) -> &str;

    fn accepts(&self, binding: &ProtocolBinding) -> bool;
}

#[derive(Default)]
pub struct ProtocolAdapterRegistry {
    adapters: Vec<Arc<dyn ProtocolAdapter>>,
}

impl ProtocolAdapterRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<A>(&mut self, adapter: A) -> Result<(), NodeError>
    where
        A: ProtocolAdapter + 'static,
    {
        if self
            .adapters
            .iter()
            .any(|item| item.protocol() == adapter.protocol())
        {
            return Err(NodeError::new(
                NodeErrorCode::Conflict,
                "protocol adapter is already registered",
            ));
        }
        self.adapters.push(Arc::new(adapter));
        Ok(())
    }

    #[must_use]
    pub fn adapter_for(&self, binding: &ProtocolBinding) -> Option<&dyn ProtocolAdapter> {
        self.adapters
            .iter()
            .find(|adapter| {
                adapter.protocol() == binding.protocol.as_str() && adapter.accepts(binding)
            })
            .map(Arc::as_ref)
    }

    #[must_use]
    pub fn supports(&self, binding: &ProtocolBinding) -> bool {
        self.adapter_for(binding).is_some()
    }

    #[must_use]
    pub fn protocols(&self) -> Vec<String> {
        self.adapters
            .iter()
            .map(|adapter| adapter.protocol().to_owned())
            .collect()
    }
}

/// A discovery mechanism such as RFC 8615, stdio, mDNS or a registry. The
/// provider returns normalized observations and never performs authorization.
pub trait DiscoveryProvider: Send + Sync {
    fn source(&self) -> DiscoverySource;

    fn discover<'a>(&'a self, target: &'a str) -> NodeFuture<'a, Vec<DiscoveryObservation>>;
}

#[derive(Default)]
pub struct DiscoveryProviderRegistry {
    providers: Vec<Arc<dyn DiscoveryProvider>>,
}

impl DiscoveryProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<P>(&mut self, provider: P) -> Result<(), NodeError>
    where
        P: DiscoveryProvider + 'static,
    {
        if self
            .providers
            .iter()
            .any(|item| item.source() == provider.source())
        {
            return Err(NodeError::new(
                NodeErrorCode::Conflict,
                "discovery provider is already registered",
            ));
        }
        self.providers.push(Arc::new(provider));
        Ok(())
    }

    pub async fn discover(
        &self,
        source: DiscoverySource,
        target: &str,
    ) -> Result<Vec<DiscoveryObservation>, NodeError> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.source() == source)
            .ok_or_else(|| {
                NodeError::new(
                    NodeErrorCode::Unsupported,
                    "discovery provider is not registered",
                )
            })?;
        let observations = provider.discover(target).await?;
        if observations.len() > MAX_DISCOVERY_OBSERVATIONS {
            return Err(NodeError::new(
                NodeErrorCode::LimitExceeded,
                "discovery provider returned too many observations",
            ));
        }
        for observation in &observations {
            if observation.source != source {
                return Err(NodeError::new(
                    NodeErrorCode::InvalidInput,
                    "discovery provider returned an observation from another source",
                ));
            }
            observation.validate()?;
        }
        Ok(observations)
    }

    #[must_use]
    pub fn sources(&self) -> Vec<DiscoverySource> {
        self.providers
            .iter()
            .map(|provider| provider.source())
            .collect()
    }
}

pub struct NodeHost {
    identity: NodeIdentity,
    manifest: NodeManifest,
    instance: Arc<dyn NodeInstance>,
    protocol_adapters: ProtocolAdapterRegistry,
}

impl NodeHost {
    pub async fn start<E>(extension: &E, identity: NodeIdentity) -> Result<Self, NodeError>
    where
        E: NodeExtension + ?Sized,
    {
        Self::start_with_adapters(extension, identity, ProtocolAdapterRegistry::new()).await
    }

    pub async fn start_with_adapters<E>(
        extension: &E,
        identity: NodeIdentity,
        protocol_adapters: ProtocolAdapterRegistry,
    ) -> Result<Self, NodeError>
    where
        E: NodeExtension + ?Sized,
    {
        identity.validate()?;
        let manifest = extension.manifest();
        manifest.validate()?;
        let context = NodeHostContext {
            identity: identity.clone(),
            extension_id: manifest.extension_id.clone(),
        };
        let instance = extension.start(context).await?;
        let capabilities = instance.capabilities();
        if capabilities != manifest.capabilities {
            // The extension has already acquired resources by this point.
            // Give it the same cleanup opportunity as a normally running host
            // instead of dropping a partially initialized instance silently.
            let _ = instance.shutdown().await;
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "node instance capabilities do not match the manifest",
            ));
        }
        Ok(Self {
            identity,
            manifest,
            instance,
            protocol_adapters,
        })
    }

    #[must_use]
    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    #[must_use]
    pub fn manifest(&self) -> &NodeManifest {
        &self.manifest
    }

    #[must_use]
    pub fn protocol_adapters(&self) -> &ProtocolAdapterRegistry {
        &self.protocol_adapters
    }

    pub fn register_protocol_adapter<A>(&mut self, adapter: A) -> Result<(), NodeError>
    where
        A: ProtocolAdapter + 'static,
    {
        self.protocol_adapters.register(adapter)
    }

    #[must_use]
    pub fn ready(&self) -> NodeReady {
        NodeReady::from_manifest(&self.identity, self.manifest.clone())
    }

    pub fn ready_line(&self) -> Result<String, NodeError> {
        encode_ready_line(&self.ready())
    }

    pub fn discovery(
        &self,
        bindings: Vec<ProtocolBinding>,
    ) -> Result<NodeDiscoveryDocument, NodeError> {
        let document =
            NodeDiscoveryDocument::from_manifest(&self.identity, self.manifest.clone(), bindings);
        document.validate()?;
        Ok(document)
    }

    /// Advertises this host's HTTP listener on the LAN. The returned handle
    /// owns the advertisement and must be retained until shutdown.
    pub fn publish_http_mdns(
        &self,
        provider: &MdnsDiscoveryProvider,
        port: u16,
    ) -> Result<MdnsAdvertisement, NodeError> {
        provider.publish_http(&self.identity, port)
    }

    pub async fn query(&self, request: NodeRequest) -> Result<NodeResponse, NodeError> {
        request.validate()?;
        let response = self.instance.query(request).await?;
        response.validate()?;
        Ok(response)
    }

    pub async fn mutate(&self, request: NodeRequest) -> Result<NodeResponse, NodeError> {
        request.validate()?;
        let response = self.instance.mutate(request).await?;
        response.validate()?;
        Ok(response)
    }

    /// Open an event stream only from a cursor belonging to this owner epoch.
    pub fn subscribe(&self, after: Option<EventCursor>) -> Result<NodeEventStream, NodeError> {
        if let Some(cursor) = &after {
            self.identity.validate_cursor(cursor)?;
        }
        Ok(self.instance.subscribe(after))
    }

    pub async fn shutdown(self) -> Result<(), NodeError> {
        self.instance.shutdown().await
    }

    #[must_use]
    pub fn instance(&self) -> &Arc<dyn NodeInstance> {
        &self.instance
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StoredNodeIdentity {
    node_id: String,
}

/// File-backed stable node identity with a fresh owner epoch for every start.
///
/// The file contains only the stable node id. The owner epoch is intentionally
/// process-generation state and is never restored from disk.
pub struct NodeIdentityStore {
    path: PathBuf,
}

impl NodeIdentityStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load_or_create(&self) -> Result<NodeIdentity, NodeError> {
        let node_id = match std::fs::read_to_string(&self.path) {
            Ok(contents) => {
                let stored: StoredNodeIdentity = serde_json::from_str(&contents).map_err(|_| {
                    NodeError::new(NodeErrorCode::InvalidInput, "invalid stored node identity")
                })?;
                stored.node_id
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => self.create_node_id()?,
            Err(error) => {
                return Err(NodeError::new(
                    NodeErrorCode::Unavailable,
                    format!("cannot read node identity: {error}"),
                ));
            }
        };
        let identity = NodeIdentity {
            node_id,
            owner_epoch: uuid::Uuid::new_v4().to_string(),
        };
        identity.validate()?;
        Ok(identity)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn create_node_id(&self) -> Result<String, NodeError> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                NodeError::new(
                    NodeErrorCode::Unavailable,
                    format!("cannot create node identity directory: {error}"),
                )
            })?;
        }
        let node_id = uuid::Uuid::new_v4().to_string();
        let contents = serde_json::to_vec(&StoredNodeIdentity {
            node_id: node_id.clone(),
        })
        .map_err(|_| NodeError::new(NodeErrorCode::Internal, "cannot encode node identity"))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(&contents).map_err(|error| {
                    NodeError::new(
                        NodeErrorCode::Unavailable,
                        format!("cannot write node identity: {error}"),
                    )
                })?;
                Ok(node_id)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stored = std::fs::read_to_string(&self.path).map_err(|read_error| {
                    NodeError::new(
                        NodeErrorCode::Unavailable,
                        format!("cannot read concurrently-created node identity: {read_error}"),
                    )
                })?;
                let stored: StoredNodeIdentity = serde_json::from_str(&stored).map_err(|_| {
                    NodeError::new(NodeErrorCode::InvalidInput, "invalid stored node identity")
                })?;
                Ok(stored.node_id)
            }
            Err(error) => Err(NodeError::new(
                NodeErrorCode::Unavailable,
                format!("cannot create node identity: {error}"),
            )),
        }
    }
}

/// Convert a ready frame into the bounded JSON text used by stdio adapters.
pub fn encode_ready(ready: &NodeReady) -> Result<String, NodeError> {
    ready.validate()?;
    let encoded = serde_json::to_string(ready)
        .map_err(|_| NodeError::new(NodeErrorCode::Internal, "cannot encode node ready frame"))?;
    if encoded.len() > MAX_READY_BYTES {
        return Err(NodeError::new(
            NodeErrorCode::LimitExceeded,
            "node ready frame is too large",
        ));
    }
    Ok(encoded)
}

/// Encode one JSONL stdio frame, including its line terminator.
pub fn encode_ready_line(ready: &NodeReady) -> Result<String, NodeError> {
    let mut encoded = encode_ready(ready)?;
    if encoded.len() + 1 > MAX_READY_BYTES {
        return Err(NodeError::new(
            NodeErrorCode::LimitExceeded,
            "node ready JSONL frame is too large",
        ));
    }
    encoded.push('\n');
    Ok(encoded)
}

/// Decode a single stdio ready frame without accepting an unrelated JSON value.
pub fn decode_ready(text: &str) -> Result<NodeReady, NodeError> {
    if text.len() > MAX_READY_BYTES {
        return Err(NodeError::new(
            NodeErrorCode::LimitExceeded,
            "node ready frame is too large",
        ));
    }
    let ready: NodeReady = serde_json::from_str(text)
        .map_err(|_| NodeError::new(NodeErrorCode::InvalidInput, "invalid node ready frame"))?;
    ready.validate()?;
    Ok(ready)
}

/// Decode one JSONL stdio frame. Additional lines are rejected instead of
/// being treated as a second command on the same frame.
pub fn decode_ready_line(line: &str) -> Result<NodeReady, NodeError> {
    if line.len() > MAX_READY_BYTES {
        return Err(NodeError::new(
            NodeErrorCode::LimitExceeded,
            "node ready JSONL frame is too large",
        ));
    }
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.contains('\r') || line.contains('\n') {
        return Err(NodeError::new(
            NodeErrorCode::InvalidInput,
            "node ready input contains multiple lines",
        ));
    }
    decode_ready(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use rpi_node_core::{NodeEvent, NodeRole, NodeTransport, ProtocolBinding, PROTOCOL, VERSION};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FixtureExtension {
        stopped: Arc<AtomicBool>,
        capabilities_match: bool,
    }

    struct FixtureInstance {
        stopped: Arc<AtomicBool>,
        capabilities_match: bool,
    }

    impl NodeExtension for FixtureExtension {
        fn manifest(&self) -> NodeManifest {
            NodeManifest {
                extension_id: "fixture.node".into(),
                extension_version: "0.1.0".into(),
                protocol: PROTOCOL.into(),
                protocol_version: VERSION,
                roles: vec![NodeRole::Workspace],
                bindings: vec![
                    ProtocolBinding {
                        protocol: PROTOCOL.into(),
                        version: VERSION.to_string(),
                        transport: NodeTransport::stdio(),
                        descriptor: None,
                    },
                    ProtocolBinding {
                        protocol: "rpi-work-node".into(),
                        version: "1".into(),
                        transport: NodeTransport::stdio(),
                        descriptor: None,
                    },
                ],
                capabilities: vec![CapabilityBinding {
                    namespace: "sessions".into(),
                    operation: "read".into(),
                    protocol: "rpi-work-node".into(),
                    version: Some("1".into()),
                }],
            }
        }

        fn start(&self, context: NodeHostContext) -> NodeFuture<'_, Arc<dyn NodeInstance>> {
            Box::pin(async move {
                assert_eq!(context.identity.node_id, "node-1");
                Ok(Arc::new(FixtureInstance {
                    stopped: self.stopped.clone(),
                    capabilities_match: self.capabilities_match,
                }) as Arc<dyn NodeInstance>)
            })
        }
    }

    impl NodeInstance for FixtureInstance {
        fn capabilities(&self) -> Vec<CapabilityBinding> {
            if self.capabilities_match {
                vec![CapabilityBinding {
                    namespace: "sessions".into(),
                    operation: "read".into(),
                    protocol: "rpi-work-node".into(),
                    version: Some("1".into()),
                }]
            } else {
                Vec::new()
            }
        }

        fn query<'a>(&'a self, request: NodeRequest) -> NodeFuture<'a, NodeResponse> {
            Box::pin(async move {
                request.validate()?;
                Ok(NodeResponse {
                    request_id: request.request_id,
                    payload: serde_json::json!({"ok": true}),
                })
            })
        }

        fn mutate<'a>(&'a self, request: NodeRequest) -> NodeFuture<'a, NodeResponse> {
            self.query(request)
        }

        fn subscribe(&self, _after: Option<EventCursor>) -> NodeEventStream {
            Box::pin(stream::empty::<Result<NodeEvent, NodeError>>())
        }

        fn shutdown<'a>(&'a self) -> NodeFuture<'a, ()> {
            Box::pin(async move {
                self.stopped.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn host_checks_manifest_and_emits_ready() {
        let stopped = Arc::new(AtomicBool::new(false));
        let extension = FixtureExtension {
            stopped: stopped.clone(),
            capabilities_match: true,
        };
        let host = NodeHost::start(
            &extension,
            NodeIdentity {
                node_id: "node-1".into(),
                owner_epoch: "epoch-1".into(),
            },
        )
        .await
        .unwrap();
        let ready = host.ready();
        assert_eq!(ready.message_type, rpi_node_core::READY_TYPE);
        assert_eq!(decode_ready(&encode_ready(&ready).unwrap()).unwrap(), ready);
        assert_eq!(
            decode_ready_line(&host.ready_line().unwrap()).unwrap(),
            ready
        );
        assert_eq!(
            decode_ready_line(&encode_ready_line(&ready).unwrap()).unwrap(),
            ready
        );
        assert_eq!(
            decode_ready_line("{}\n{}").unwrap_err().code,
            NodeErrorCode::InvalidInput
        );
        assert_eq!(
            decode_ready_line(&"x".repeat(MAX_READY_BYTES + 1))
                .unwrap_err()
                .code,
            NodeErrorCode::LimitExceeded
        );
        assert_eq!(
            decode_ready_line(&format!("{}\n", "x".repeat(MAX_READY_BYTES)))
                .unwrap_err()
                .code,
            NodeErrorCode::LimitExceeded
        );
        let response = host
            .query(NodeRequest {
                request_id: "request-1".into(),
                operation: "sessions.read".into(),
                payload: serde_json::json!({"path":"/"}),
            })
            .await
            .unwrap();
        assert_eq!(response.request_id, "request-1");
        let discovery = host.discovery(ready.bindings.clone()).unwrap();
        assert_eq!(discovery.node_id, "node-1");
        discovery.validate().unwrap();
        let stale = match host.subscribe(Some(EventCursor {
            node_id: "node-1".into(),
            owner_epoch: "old-epoch".into(),
            sequence: 1,
        })) {
            Ok(_) => panic!("stale cursor was accepted"),
            Err(error) => error,
        };
        assert_eq!(stale.code, NodeErrorCode::Conflict);
        let _stream = host
            .subscribe(Some(EventCursor {
                node_id: "node-1".into(),
                owner_epoch: "epoch-1".into(),
                sequence: 1,
            }))
            .unwrap();
        host.shutdown().await.unwrap();
        assert!(stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn host_cleans_up_when_instance_capabilities_are_invalid() {
        let stopped = Arc::new(AtomicBool::new(false));
        let result = NodeHost::start(
            &FixtureExtension {
                stopped: stopped.clone(),
                capabilities_match: false,
            },
            NodeIdentity {
                node_id: "node-1".into(),
                owner_epoch: "epoch-1".into(),
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(error) if error.code == NodeErrorCode::InvalidInput
        ));
        assert!(stopped.load(Ordering::SeqCst));
    }

    struct StdioAdapter;

    impl ProtocolAdapter for StdioAdapter {
        fn protocol(&self) -> &str {
            PROTOCOL
        }

        fn accepts(&self, binding: &ProtocolBinding) -> bool {
            binding.version == VERSION.to_string()
        }
    }

    #[test]
    fn protocol_registry_keeps_foreign_protocols_optional() {
        let mut registry = ProtocolAdapterRegistry::new();
        registry.register(StdioAdapter).unwrap();
        let binding = ProtocolBinding {
            protocol: PROTOCOL.into(),
            version: VERSION.to_string(),
            transport: NodeTransport::stdio(),
            descriptor: None,
        };
        assert!(registry.supports(&binding));
        assert_eq!(registry.adapter_for(&binding).unwrap().protocol(), PROTOCOL);
        assert_eq!(registry.protocols(), vec![PROTOCOL]);
    }

    struct FixtureDiscovery;

    impl DiscoveryProvider for FixtureDiscovery {
        fn source(&self) -> DiscoverySource {
            DiscoverySource::Registry
        }

        fn discover<'a>(&'a self, target: &'a str) -> NodeFuture<'a, Vec<DiscoveryObservation>> {
            Box::pin(async move {
                Ok(vec![DiscoveryObservation {
                    source: DiscoverySource::Registry,
                    transport: None,
                    identity: None,
                    manifest: None,
                    external_descriptor: Some(rpi_node_core::ExternalDescriptor {
                        protocol: "registry".into(),
                        version: None,
                        payload: serde_json::json!({"target": target}),
                    }),
                }])
            })
        }
    }

    #[tokio::test]
    async fn discovery_registry_validates_provider_observations() {
        let mut registry = DiscoveryProviderRegistry::new();
        registry.register(FixtureDiscovery).unwrap();
        let observations = registry
            .discover(DiscoverySource::Registry, "service://fixture")
            .await
            .unwrap();
        assert_eq!(registry.sources(), vec![DiscoverySource::Registry]);
        assert_eq!(observations.len(), 1);
    }

    struct OverflowDiscovery;

    impl DiscoveryProvider for OverflowDiscovery {
        fn source(&self) -> DiscoverySource {
            DiscoverySource::Mdns
        }

        fn discover<'a>(&'a self, _target: &'a str) -> NodeFuture<'a, Vec<DiscoveryObservation>> {
            Box::pin(async {
                Ok(vec![
                    DiscoveryObservation {
                        source: DiscoverySource::Mdns,
                        transport: None,
                        identity: None,
                        manifest: None,
                        external_descriptor: None,
                    };
                    rpi_node_core::MAX_DISCOVERY_OBSERVATIONS + 1
                ])
            })
        }
    }

    #[tokio::test]
    async fn discovery_registry_bounds_provider_results() {
        let mut registry = DiscoveryProviderRegistry::new();
        registry.register(OverflowDiscovery).unwrap();
        let result = registry
            .discover(DiscoverySource::Mdns, "_rpi-node._tcp")
            .await;
        assert_eq!(result.unwrap_err().code, NodeErrorCode::LimitExceeded);
    }

    #[test]
    fn identity_store_keeps_node_id_and_changes_epoch() {
        let root = std::env::temp_dir().join(format!("rpi-node-test-{}", uuid::Uuid::new_v4()));
        let path = root.join("node-id.json");
        let store = NodeIdentityStore::new(&path);
        let first = store.load_or_create().unwrap();
        let second = store.load_or_create().unwrap();
        assert_eq!(first.node_id, second.node_id);
        assert_ne!(first.owner_epoch, second.owner_epoch);
        std::fs::remove_dir_all(root).unwrap();
    }
}
