//! Trusted, opt-in MCP HTTP OAuth provisioning. Never reads model metadata or
//! native agent authentication files. The provider owns callbacks and tokens.
use crate::protocol::Writer;
use rpi_extcore::mcp::{
    oauth::{
        AuthorizationFuture, AuthorizationInteraction, AuthorizationOutcome, AuthorizationRequest,
        NetworkPolicy, OAuthNetwork, OAuthOptions, OAuthProvider, ResourcePolicy,
    },
    AuthError, McpOptions,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::Read,
    net::IpAddr,
    path::Path,
    sync::{Arc, Weak},
};

const MAX_POLICY: u64 = 64 * 1024;
const INVALID: &str =
    "invalid RPI_ACP_MCP_OAUTH_POLICY (expected strict resource and network policy)";
const UNREADABLE: &str =
    "RPI_ACP_MCP_OAUTH_POLICY requires an absolute readable JSON file <=64 KiB";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Resource {
    resource: String,
    issuer: String,
    scopes: Vec<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    allow_dynamic_registration: bool,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrivateHttps {
    origin: String,
    ips: Vec<IpAddr>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Network {
    origins: Vec<String>,
    #[serde(default)]
    development_loopback_origins: Vec<String>,
    #[serde(default)]
    private_https: Vec<PrivateHttps>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Policy {
    resources: Vec<Resource>,
    network: Network,
}
impl Policy {
    fn provider(
        &self,
        interaction: Arc<dyn AuthorizationInteraction>,
    ) -> Result<OAuthProvider, AuthError> {
        let mut network = NetworkPolicy::new(self.network.origins.clone())?;
        for origin in &self.network.development_loopback_origins {
            network.allow_development_origin(origin)?;
        }
        for grant in &self.network.private_https {
            network.allow_private_origin(&grant.origin, grant.ips.clone())?;
        }
        let mut origins = self.network.origins.clone();
        origins.extend(self.network.development_loopback_origins.clone());
        origins.extend(self.network.private_https.iter().map(|g| g.origin.clone()));
        if origins.iter().any(|o| o.contains('*')) {
            return Err(AuthError::UnsafeUrl);
        }
        // Resource/issuer identities must be within explicitly provisioned origins
        // before any discovery request, not just fail later at the connector.
        for resource in &self.resources {
            for value in [&resource.resource, &resource.issuer] {
                let url = url::Url::parse(value).map_err(|_| AuthError::UnsafeUrl)?;
                if !origins
                    .iter()
                    .any(|o| url::Url::parse(o).is_ok_and(|o| o.origin() == url.origin()))
                {
                    return Err(AuthError::UnsafeUrl);
                }
            }
        }
        OAuthProvider::new(
            OAuthOptions {
                resources: self
                    .resources
                    .iter()
                    .map(|r| ResourcePolicy {
                        resource: r.resource.clone(),
                        issuer: r.issuer.clone(),
                        scopes: r.scopes.clone(),
                        client_id: r.client_id.clone(),
                        allow_dynamic_registration: r.allow_dynamic_registration,
                    })
                    .collect(),
                max_bindings: 256,
                authorization_origins: origins,
            },
            OAuthNetwork::new(network)?,
            interaction,
        )
    }
    fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_POLICY as usize {
            return Err(UNREADABLE);
        }
        let policy: Self = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        policy
            .provider(Arc::new(Unavailable))
            .map_err(|_| INVALID)?;
        Ok(policy)
    }
}
struct Unavailable;
impl AuthorizationInteraction for Unavailable {
    fn authorize<'a>(&'a self, _: AuthorizationRequest<'a>) -> AuthorizationFuture<'a> {
        Box::pin(async { Err(AuthError::Rejected) })
    }
}

pub(crate) type Provision = Result<Option<Arc<Policy>>, &'static str>;
/// Called once by the connection's writer constructor. Errors remain fixed and
/// contain no path, file content, credentials or remote diagnostics.
pub(crate) fn load() -> Provision {
    let Some(path) = std::env::var_os("RPI_ACP_MCP_OAUTH_POLICY") else {
        return Ok(None);
    };
    load_file(Path::new(&path)).map(|p| Some(Arc::new(p)))
}
fn load_file(path: &Path) -> Result<Policy, &'static str> {
    if !path.is_absolute() {
        return Err(UNREADABLE);
    }
    let file = std::fs::File::open(path).map_err(|_| UNREADABLE)?;
    let meta = file.metadata().map_err(|_| UNREADABLE)?;
    if !meta.is_file() || meta.len() > MAX_POLICY {
        return Err(UNREADABLE);
    }
    let mut bytes = Vec::new();
    file.take(MAX_POLICY + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNREADABLE)?;
    Policy::parse(&bytes)
}

pub(crate) fn options(writer: &Arc<Writer>, session_id: &str) -> Result<McpOptions, String> {
    let policy = writer
        .mcp_oauth_policy
        .as_ref()
        .map_err(|e| (*e).to_owned())?;
    let Some(policy) = policy else {
        return Ok(McpOptions::default());
    };
    if !writer.supports_elicitation_url() {
        return Err("managed MCP OAuth requires negotiated URL elicitation".into());
    }
    let interaction = Arc::new(Interaction {
        writer: Arc::downgrade(writer),
        session_id: session_id.into(),
    });
    let provider = policy.provider(interaction).map_err(|_| INVALID)?;
    Ok(McpOptions {
        credential_provider: Some(Arc::new(provider)),
        ..Default::default()
    })
}
struct Interaction {
    writer: Weak<Writer>,
    session_id: String,
}
impl AuthorizationInteraction for Interaction {
    fn authorize<'a>(&'a self, request: AuthorizationRequest<'a>) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            let writer = self.writer.upgrade().ok_or(AuthError::Cancelled)?;
            if !writer.supports_elicitation_url() {
                return Err(AuthError::Rejected);
            }
            let answer = writer
                .request_mcp_oauth(
                    json!({
                        "sessionId": self.session_id,
                        "mode": "url", "message": consent_message(&request.binding.resource, request.issuer, request.scopes),
                        "url": request.authorization_url.as_str(),
                        "elicitationId": uuid::Uuid::new_v4().to_string(),
                    }),
                    request.control,
                    request.deadline,
                )
                .await?;
            authorization_outcome(answer)
        })
    }
}
/// Only trusted grant identities, never the authorization URL's code/state or tokens.
fn consent_message(resource: &str, issuer: &str, scopes: &[String]) -> String {
    format!(
        "Authorize MCP access\nResource: {}\nIssuer: {}\nScopes: {}",
        resource,
        issuer,
        if scopes.is_empty() { "(none)".into() } else { scopes.join(", ") },
    )
}
fn authorization_outcome(answer: serde_json::Value) -> Result<AuthorizationOutcome, AuthError> {
    match answer.get("action").and_then(serde_json::Value::as_str) {
        Some("accept") => Ok(AuthorizationOutcome::Accepted),
        Some("decline") => Ok(AuthorizationOutcome::Declined),
        Some("cancel") => Err(AuthError::Cancelled),
        _ => Err(AuthError::Rejected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> serde_json::Value {
        json!({"resources":[{"resource":"https://mcp.example/mcp", "issuer":"https://auth.example/", "scopes":["tools"], "clientId":"public"}], "network":{"origins":["https://mcp.example","https://auth.example"]}})
    }
    #[test]
    fn strict_policy_and_fixed_errors() {
        assert!(Policy::parse(&serde_json::to_vec(&valid()).unwrap()).is_ok());
        for value in [json!({}), json!({"resources":[],"network":{"origins":[]}})] {
            assert!(Policy::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for pointer in ["", "/resources/0", "/network"] {
            let mut value = valid();
            value.pointer_mut(pointer).unwrap()["unknown"] = json!("secret");
            assert_eq!(
                Policy::parse(&serde_json::to_vec(&value).unwrap()).err(),
                Some(INVALID)
            );
        }
        for origin in [
            "*",
            "http://auth.example",
            "https://user:secret@auth.example",
            "https://auth.example/path",
        ] {
            let mut value = valid();
            value["network"]["origins"] = json!([origin]);
            assert!(Policy::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let mut value = valid();
        value["network"]["developmentLoopbackOrigins"] = json!(["http://localhost:3000"]);
        assert!(Policy::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut value = valid();
        value["network"]["privateHttps"] =
            json!([{"origin":"https://auth.example", "ips":["127.0.0.1/8"]}]);
        assert!(Policy::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        assert_eq!(Policy::parse(&vec![b' '; 65537]).err(), Some(UNREADABLE));
        assert_eq!(
            load_file(Path::new("relative.json")).err(),
            Some(UNREADABLE)
        );
    }
    #[test]
    fn ui_actions_are_not_credentials_and_policy_is_required() {
        assert_eq!(
            authorization_outcome(json!({"action":"accept"})),
            Ok(AuthorizationOutcome::Accepted)
        );
        assert_eq!(
            authorization_outcome(json!({"action":"decline"})),
            Ok(AuthorizationOutcome::Declined)
        );
        assert_eq!(
            authorization_outcome(json!({"action":"cancel"})),
            Err(AuthError::Cancelled)
        );
        assert_eq!(
            authorization_outcome(json!({"authenticated":true})),
            Err(AuthError::Rejected)
        );
        let mut value = valid();
        value["network"]["origins"]
            .as_array_mut()
            .unwrap()
            .push(json!("https://*.example"));
        assert_eq!(
            Policy::parse(&serde_json::to_vec(&value).unwrap()).err(),
            Some(INVALID)
        );
        let mut writer = Writer::with_output(tokio::io::sink());
        writer.mcp_oauth_policy = Ok(Some(Arc::new(
            Policy::parse(&serde_json::to_vec(&valid()).unwrap()).unwrap(),
        )));
        let writer = Arc::new(writer);
        assert!(options(&writer, "native-root").is_err());
        writer.negotiate_questions(&json!({"clientCapabilities":{"elicitation":{"url":{}}}}));
        assert!(options(&writer, "native-root")
            .unwrap()
            .credential_provider
            .is_some());
    }
    #[test]
    fn consent_names_trusted_grant_not_authorization_secrets() {
        assert_eq!(
            consent_message("https://mcp.example/mcp", "https://auth.example/", &["read".into(), "tools".into()]),
            "Authorize MCP access\nResource: https://mcp.example/mcp\nIssuer: https://auth.example/\nScopes: read, tools"
        );
        assert!(consent_message("resource", "issuer", &[]).ends_with("Scopes: (none)"));
    }

    #[tokio::test]
    async fn dropped_oauth_request_joins_exact_id_cancellation() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (output, input) = tokio::io::duplex(8192);
        let writer = Arc::new(Writer::with_output(output));
        let mut lines = BufReader::new(input).lines();
        let owner = writer.clone();
        let task = tokio::spawn(async move {
            let control = rpi_extcore::mcp::McpControl::default();
            owner.request_mcp_oauth(
                json!({"sessionId":"root", "elicitationId":"not-the-request-id", "mode":"url"}),
                &control, std::time::Instant::now() + std::time::Duration::from_secs(5),
            ).await
        });
        let frame: serde_json::Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        writer.join_oauth_cleanup().await;
        let cancel: serde_json::Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(cancel["method"], "$/cancel_request");
        assert_eq!(cancel["params"]["requestId"], frame["id"]);
        assert_ne!(cancel["params"]["requestId"], frame["params"]["elicitationId"]);
    }

    #[test]
    fn absent_policy_is_noop_and_does_not_advertise_auth() {
        let writer = Arc::new(Writer::with_output(tokio::io::sink()));
        assert!(options(&writer, "root")
            .unwrap()
            .credential_provider
            .is_none());
        assert!(!writer.supports_elicitation_url());
    }
}
