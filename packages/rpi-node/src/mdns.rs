//! Multicast DNS service discovery for nodes on the local network.
//!
//! mDNS is deliberately used as a locator only. A discovered service should
//! still be fetched through `/.well-known/rpi-node` and authenticated before
//! any operation is attempted.

use std::{collections::HashSet, time::Duration};

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use rpi_node_core::{
    DiscoveryObservation, DiscoverySource, NodeError, NodeErrorCode, NodeIdentity, NodeTransport,
    DISCOVERY_PATH, PROTOCOL, VERSION,
};
use tokio::sync::Mutex;

use crate::{DiscoveryProvider, NodeFuture};

/// DNS-SD service type advertised by an HTTP-capable rpi node.
pub const MDNS_SERVICE_TYPE: &str = "_rpi-node._tcp.local.";

/// Default time spent collecting mDNS responses for one discovery request.
pub const DEFAULT_MDNS_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// A mDNS browser and optional node advertiser sharing one daemon.
///
/// The daemon owns a background thread from `mdns-sd`. Dropping this provider
/// shuts that thread down. Keep the provider alive for as long as its
/// advertisements should remain visible.
pub struct MdnsDiscoveryProvider {
    daemon: ServiceDaemon,
    receiver: Mutex<mdns_sd::Receiver<ServiceEvent>>,
    timeout: Duration,
}

impl MdnsDiscoveryProvider {
    /// Starts a standard-port mDNS daemon and begins browsing `_rpi-node`.
    pub fn new() -> Result<Self, NodeError> {
        Self::with_timeout(DEFAULT_MDNS_DISCOVERY_TIMEOUT)
    }

    /// Starts a browser with an explicit response collection timeout.
    pub fn with_timeout(timeout: Duration) -> Result<Self, NodeError> {
        if timeout.is_zero() {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "mDNS discovery timeout must be non-zero",
            ));
        }
        let daemon = ServiceDaemon::new().map_err(|error| {
            NodeError::new(
                NodeErrorCode::Unavailable,
                format!("cannot start mDNS daemon: {error}"),
            )
        })?;
        let receiver = daemon.browse(MDNS_SERVICE_TYPE).map_err(|error| {
            let _ = daemon.shutdown();
            NodeError::new(
                NodeErrorCode::Unavailable,
                format!("cannot browse mDNS node service: {error}"),
            )
        })?;
        Ok(Self {
            daemon,
            receiver: Mutex::new(receiver),
            timeout,
        })
    }

    /// Publishes an HTTP endpoint using a deterministic `.local.` host name.
    ///
    /// This is the zero-configuration path for a node. The returned handle
    /// must be retained while the service should be advertised.
    pub fn publish_http(
        &self,
        identity: &NodeIdentity,
        port: u16,
    ) -> Result<MdnsAdvertisement, NodeError> {
        let host_name = format!("{}.local.", instance_name(&identity.node_id));
        self.publish(identity, port, host_name)
    }

    /// Publishes the HTTP endpoint for a node on the local network.
    ///
    /// The returned handle must be retained while the service should be
    /// advertised. The service includes only locator metadata; clients must
    /// retrieve and validate the full discovery document over HTTP.
    pub fn publish(
        &self,
        identity: &NodeIdentity,
        port: u16,
        host_name: impl Into<String>,
    ) -> Result<MdnsAdvertisement, NodeError> {
        identity.validate()?;
        if port == 0 {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "mDNS advertised port must be non-zero",
            ));
        }
        let host_name = host_name.into();
        if host_name.trim().is_empty() {
            return Err(NodeError::new(
                NodeErrorCode::InvalidInput,
                "mDNS host name must not be empty",
            ));
        }

        let instance_name = instance_name(&identity.node_id);
        let properties = vec![
            ("protocol".to_owned(), PROTOCOL.to_owned()),
            ("version".to_owned(), VERSION.to_string()),
            ("node_id".to_owned(), identity.node_id.clone()),
            ("owner_epoch".to_owned(), identity.owner_epoch.clone()),
            ("path".to_owned(), DISCOVERY_PATH.to_owned()),
        ];
        let service = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &instance_name,
            &host_name,
            "",
            port,
            properties.as_slice(),
        )
        .map_err(|error| {
            NodeError::new(
                NodeErrorCode::InvalidInput,
                format!("invalid mDNS service information: {error}"),
            )
        })?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_owned();
        self.daemon.register(service).map_err(|error| {
            NodeError::new(
                NodeErrorCode::Unavailable,
                format!("cannot publish mDNS node service: {error}"),
            )
        })?;
        Ok(MdnsAdvertisement {
            daemon: self.daemon.clone(),
            fullname,
            active: true,
        })
    }
}

impl DiscoveryProvider for MdnsDiscoveryProvider {
    fn source(&self) -> DiscoverySource {
        DiscoverySource::Mdns
    }

    fn discover<'a>(&'a self, target: &'a str) -> NodeFuture<'a, Vec<DiscoveryObservation>> {
        Box::pin(async move {
            if !target.is_empty() && target != MDNS_SERVICE_TYPE {
                return Err(NodeError::new(
                    NodeErrorCode::InvalidInput,
                    "mDNS provider only supports the _rpi-node._tcp.local. service",
                ));
            }

            let deadline = tokio::time::Instant::now() + self.timeout;
            let receiver = self.receiver.lock().await;
            let mut observations = Vec::new();
            let mut seen = HashSet::new();
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let event = match tokio::time::timeout(remaining, receiver.recv_async()).await {
                    Ok(Ok(event)) => event,
                    Ok(Err(_)) => {
                        return Err(NodeError::new(
                            NodeErrorCode::Unavailable,
                            "mDNS browse channel closed",
                        ));
                    }
                    Err(_) => break,
                };
                if let ServiceEvent::ServiceResolved(service) = event {
                    if let Some(observation) = observation_from_service(&service) {
                        if seen.insert(service.fullname.clone()) {
                            observations.push(observation);
                        }
                    }
                }
            }
            Ok(observations)
        })
    }
}

impl Drop for MdnsDiscoveryProvider {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// Owns one published mDNS service. Dropping it withdraws the advertisement.
pub struct MdnsAdvertisement {
    daemon: ServiceDaemon,
    fullname: String,
    active: bool,
}

impl MdnsAdvertisement {
    /// Withdraws the advertisement immediately. Dropping also withdraws it.
    pub fn stop(mut self) -> Result<(), NodeError> {
        let result = self.unregister();
        self.active = false;
        result
    }

    fn unregister(&self) -> Result<(), NodeError> {
        self.daemon.unregister(&self.fullname).map_err(|error| {
            NodeError::new(
                NodeErrorCode::Unavailable,
                format!("cannot withdraw mDNS node service: {error}"),
            )
        })?;
        Ok(())
    }
}

impl Drop for MdnsAdvertisement {
    fn drop(&mut self) {
        if self.active {
            let _ = self.unregister();
        }
    }
}

fn observation_from_service(service: &ResolvedService) -> Option<DiscoveryObservation> {
    let address = service.get_addresses_v4().into_iter().min()?;
    let origin = format!("http://{address}:{}", service.port);
    let identity = match (
        service.get_property_val_str("node_id"),
        service.get_property_val_str("owner_epoch"),
    ) {
        (Some(node_id), Some(owner_epoch)) => {
            let identity = NodeIdentity {
                node_id: node_id.to_owned(),
                owner_epoch: owner_epoch.to_owned(),
            };
            identity.validate().ok().map(|()| identity)
        }
        _ => None,
    };
    let mut transport = NodeTransport::http(origin);
    transport.path = Some(
        service
            .get_property_val_str("path")
            .unwrap_or(DISCOVERY_PATH)
            .to_owned(),
    );
    let observation = DiscoveryObservation {
        source: DiscoverySource::Mdns,
        transport: Some(transport),
        identity,
        manifest: None,
        external_descriptor: None,
    };
    observation.validate().ok()?;
    Some(observation)
}

fn instance_name(node_id: &str) -> String {
    let mut suffix = node_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(10)
        .collect::<String>();
    if suffix.is_empty() {
        suffix.push_str("node");
    }
    format!("rpi-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::instance_name;

    #[test]
    fn instance_name_is_short_and_dns_safe() {
        let name = instance_name("node-1234567890");
        assert_eq!(name, "rpi-node123456");
        assert!(name.len() <= 15);
        assert!(name
            .chars()
            .all(|character| { character.is_ascii_alphanumeric() || character == '-' }));
    }
}
