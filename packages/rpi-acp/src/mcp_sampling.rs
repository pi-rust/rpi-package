//! Trusted connection provisioning for isolated native MCP sampling.
use crate::{mcp::Servers, protocol::Writer};
use rpi_extcore::{host::HostSession, mcp::{McpControl, McpServerConfig}, sampling::*};
use serde::Deserialize;
use serde_json::json;
use std::{collections::{BTreeMap, BTreeSet}, io::Read, path::{Path, PathBuf}, sync::{Arc, Weak, Mutex, atomic::{AtomicU32, Ordering}}, time::{Duration, Instant}};

const INVALID: &str = "invalid RPI_ACP_MCP_SAMPLING_POLICY";
pub(crate) type Provision = Result<Option<Arc<Policy>>, String>;
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct FilePolicy { resources: Vec<Resource> }
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct Resource {
    resource: String, cwd: PathBuf, max_tokens: u64, max_messages: usize,
    max_duration_seconds: u64, max_requests: u32,
    #[serde(default="yes")] require_consent: bool,
    #[serde(default)] allow_insecure_loopback: bool,
}
fn yes() -> bool { true }
struct Rule { resource: Resource, remaining: AtomicU32 }
pub(crate) struct Policy { rules: Vec<Arc<Rule>> }
fn canonical_url(text: &str, insecure: bool) -> bool {
    let Ok(u) = url::Url::parse(text) else { return false; };
    u.as_str() == text && u.host_str().is_some() && u.username().is_empty()
        && u.password().is_none() && u.fragment().is_none()
        && (u.scheme() == "https" || (u.scheme() == "http" && insecure && match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(), _ => false,
        }))
}
impl Policy {
    fn parse(bytes: &[u8]) -> Result<Arc<Self>, String> {
        if bytes.len() > 65536 { return Err(INVALID.into()); }
        let file: FilePolicy = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        let mut seen = BTreeSet::new();
        let mut rules = Vec::new();
        for mut r in file.resources {
            if !canonical_url(&r.resource, r.allow_insecure_loopback)
                || !r.cwd.is_absolute() || !r.cwd.is_dir()
                || !(1..=16384).contains(&r.max_tokens) || !(1..=128).contains(&r.max_messages)
                || !(1..=300).contains(&r.max_duration_seconds) || !(1..=1024).contains(&r.max_requests) {
                return Err(INVALID.into());
            }
            // Normalize caller paths once; Windows canonicalization may add a
            // verbatim prefix to an otherwise valid absolute policy path.
            r.cwd = std::fs::canonicalize(&r.cwd).map_err(|_| INVALID)?;
            if !seen.insert((r.resource.clone(), r.cwd.clone())) { return Err(INVALID.into()); }
            rules.push(Arc::new(Rule { remaining: AtomicU32::new(r.max_requests), resource: r }));
        }
        Ok(Arc::new(Self { rules }))
    }
}
pub(crate) fn load() -> Provision {
    let Some(path) = std::env::var_os("RPI_ACP_MCP_SAMPLING_POLICY") else { return Ok(None); };
    load_path(Path::new(&path)).map(Some)
}
fn load_path(path: &Path) -> Result<Arc<Policy>, String> {
    if !path.is_absolute() { return Err(INVALID.into()); }
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(|_| INVALID)?.take(65537).read_to_end(&mut bytes).map_err(|_| INVALID)?;
    Policy::parse(&bytes)
}
/// Local configuration is the authority; remote target/model/metadata is never consulted.
pub(crate) fn configured(servers: &Servers) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (name, config) in &servers.0 {
        if let McpServerConfig::Http(http) = config {
            if http.enabled { map.insert(name.clone(), http.url.clone()); }
        }
    }
    for id in servers.1.values() {
        if canonical_url(id, true) { map.insert(id.clone(), id.clone()); }
    }
    map
}
struct Authorizer {
    writer: Weak<Writer>, scope: String, cwd: PathBuf,
    servers: BTreeMap<String, Arc<Rule>>,
    // Core owns connection identities. Pin first locally produced identity, never
    // interpret target (native HTTP target is a private config-file path).
    identities: Mutex<BTreeMap<String, (String, u64)>>,
    model: Option<String>,
}
pub(crate) fn options(writer: &Arc<Writer>, host: &Arc<HostSession>, scope: &str, cwd: &Path,
    servers: &Servers, owner: &mut Option<NativeSamplingOwner>) -> Result<Option<SamplingOptions>, String> {
    let configured = configured(servers);
    if configured.is_empty() { return Ok(None); }
    let Some(policy) = writer.mcp_sampling_policy.as_ref().map_err(Clone::clone)? else { return Ok(None); };
    let cwd = std::fs::canonicalize(cwd).map_err(|_| INVALID)?;
    let grants = configured.into_iter().filter_map(|(name, url)| {
        policy.rules.iter().find(|r| r.resource.resource == url && r.resource.cwd == cwd
            && (!r.resource.require_consent || writer.supports_elicitation_form()))
            .map(|r| (name, r.clone()))
    }).collect::<BTreeMap<_, _>>();
    if grants.is_empty() { return Ok(None); }
    if owner.is_none() { *owner = Some(NativeSamplingOwner::new(host, scope.into(), cwd.clone())?); }
    let model = host.ready().get("model").and_then(|v| v.as_str()).map(str::to_owned);
    Ok(Some(SamplingOptions { executor: owner.as_ref().unwrap().executor(), authorizer: Arc::new(Authorizer {
        writer: Arc::downgrade(writer), scope: scope.into(), cwd, servers: grants,
        identities: Mutex::new(BTreeMap::new()), model,
    }) }))
}
fn check(context: &SamplingContext, control: &McpControl) -> Result<(), SamplingError> {
    control.check().map_err(|_| SamplingError::Cancelled)?;
    if Instant::now() >= context.deadline { return Err(SamplingError::TimedOut); }
    Ok(())
}
fn approved(answer: &serde_json::Value) -> bool {
    answer["action"] == "accept" && answer["content"].as_object().is_some_and(|o|
        o.len() == 1 && o.get("approved") == Some(&json!(true)))
}
fn preview(request: &SamplingRequest) -> String {
    let text = serde_json::to_string(request.as_value()).unwrap_or_default();
    let mut end = text.len().min(2048);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].to_owned()
}
impl SamplingAuthorizer for Authorizer {
    fn authorize<'a>(&'a self, context: &'a SamplingContext, request: &'a SamplingRequest, control: &'a McpControl) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            check(context, control)?;
            let rule = self.servers.get(&context.server).ok_or(SamplingError::Denied)?;
            let r = &rule.resource;
            if context.scope != self.scope || context.cwd != self.cwd || context.namespace.is_empty()
                || context.generation == 0 || context.operation.is_empty()
                || request.max_tokens() > r.max_tokens || request.messages().len() > r.max_messages {
                return Err(SamplingError::Denied);
            }
            {
                let mut identities = self.identities.lock().unwrap();
                let identity = (context.namespace.clone(), context.generation);
                if identities.get(&context.server).is_some_and(|old| old != &identity) { return Err(SamplingError::Denied); }
                identities.entry(context.server.clone()).or_insert(identity);
            }
            // Only shorten the original absolute deadline, never extend/reset it.
            // Consent and model execution consume this single trusted budget.
            let max_duration = Duration::from_secs(r.max_duration_seconds);
            let mut granted = context.clone();
            granted.deadline = context.deadline.min(Instant::now() + max_duration);
            // Attempt budget is shared across roots and consumed BEFORE any UI.
            #[allow(deprecated)] // fetch_update is required by ACP MSRV 1.85.
            rule.remaining.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1)).map_err(|_| SamplingError::Denied)?;
            if r.require_consent {
                let writer = self.writer.upgrade().ok_or(SamplingError::Unavailable)?;
                if !writer.supports_elicitation_form() { return Err(SamplingError::Denied); }
                let message = format!("Allow isolated MCP sampling? Configured server: {}\nResource: {}\nCurrent configured model: {}\nToken maximum: {}\nUNTRUSTED REMOTE PROMPT (preview, not foreground):\n{}", context.server, r.resource, self.model.as_deref().unwrap_or("unknown"), request.max_tokens(), preview(request));
                let answer = writer.request_mcp_oauth(json!({"sessionId":self.scope,"mode":"form","message":message,
                    "requestedSchema":{"type":"object","properties":{"approved":{"type":"boolean"}},"required":["approved"],"additionalProperties":false}}), control, granted.deadline).await.map_err(|_| SamplingError::Denied)?;
                if !approved(&answer) { return Err(SamplingError::Denied); }
            }
            check(context, control)?;
            check(&granted, control)?;
            Ok(SamplingPolicy { context: granted.clone(), max_messages: r.max_messages, max_tokens: r.max_tokens, max_duration })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn file(consent: bool, attempts: u32) -> Value {
        json!({"resources":[{"resource":"https://mcp.example/mcp","cwd":std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
            "maxTokens":32,"maxMessages":2,"maxDurationSeconds":300,"maxRequests":attempts,"requireConsent":consent}]})
    }
    fn policy(value: Value) -> Arc<Policy> { Policy::parse(&serde_json::to_vec(&value).unwrap()).unwrap() }
    fn auth(p: &Arc<Policy>, scope: &str) -> Authorizer {
        Authorizer { writer: Weak::new(), scope:scope.into(), cwd:p.rules[0].resource.cwd.clone(),
            servers:BTreeMap::from([("local".into(),p.rules[0].clone())]), identities:Mutex::new(BTreeMap::new()), model:None }
    }
    fn context(a: &Authorizer) -> SamplingContext {
        SamplingContext { cwd:a.cwd.clone(), scope:a.scope.clone(), namespace:"trusted-core-namespace".into(),
            server:"local".into(), target:"private/config/path".into(), generation:7, operation:"op".into(), deadline:Instant::now()+Duration::from_secs(2) }
    }
    fn request() -> SamplingRequest {
        SamplingRequest::parse(json!({"maxTokens":32,"messages":[{"role":"user","content":{"type":"text","text":"untrusted"}}],"modelPreferences":{"hints":[{"name":"not-a-grant"}]}})).unwrap()
    }
    #[test]
    fn consent_requires_exact_boolean_approval() {
        assert!(approved(&json!({"action":"accept","content":{"approved":true}})));
        for answer in [json!({"action":"decline","content":{"approved":true}}), json!({"action":"cancel"}),
            json!({"action":"accept","content":{"approved":"true"}}), json!({"action":"accept","content":{"approved":false}}),
            json!({"action":"accept","content":{"approved":true,"extra":true}}), json!({"action":"accept"})] {
            assert!(!approved(&answer));
        }
    }
    #[test]
    fn strict_policy_bounds_and_exact_identities() {
        assert!(Policy::parse(&vec![b' ';65537]).is_err());
        assert!(load_path(Path::new("relative.json")).is_err());
        let valid = file(false, 2);
        assert!(Policy::parse(&serde_json::to_vec(&valid).unwrap()).is_ok());
        let mut ordinary_path = valid.clone();
        ordinary_path["resources"][0]["cwd"] = json!(std::env::current_dir().unwrap());
        assert_eq!(policy(ordinary_path).rules[0].resource.cwd,
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap());
        for (key, value) in [("maxTokens",json!(0)),("maxMessages",json!(129)),("maxDurationSeconds",json!(301)),("maxRequests",json!(1025)),("unknown",json!(true)),("resource",json!("https://MCP.example/mcp")),("cwd",json!("relative"))] {
            let mut bad=valid.clone(); bad["resources"][0][key]=value;
            assert!(Policy::parse(&serde_json::to_vec(&bad).unwrap()).is_err(), "{key}");
        }
        assert!(!canonical_url("http://localhost/mcp",true));
        assert!(!canonical_url("http://127.0.0.1/mcp",false));
        assert!(canonical_url("http://127.0.0.1/mcp",true));
        assert!(canonical_url("http://[::1]/mcp",true));
        assert!(!canonical_url("http://192.168.1.1/mcp",true));
        assert!(!canonical_url("https://user:secret@mcp.example/mcp",false));
        let mut default=file(true,1); default["resources"][0].as_object_mut().unwrap().remove("requireConsent");
        assert!(policy(default).rules[0].resource.require_consent);
    }
    #[tokio::test]
    async fn preapproval_shared_sibling_budget_and_context_denials() {
        let p=policy(file(false,2)); let a=auth(&p,"root"); let b=auth(&p,"sibling");
        let c=context(&a); let req=request(); let control=McpControl::default();
        for change in ["server","scope","cwd","namespace","generation","tokens","deadline"] {
            let mut bad=c.clone();
            match change {
                "server"=>bad.server="remote-asked".into(), "scope"=>bad.scope="sibling".into(),
                "cwd"=>bad.cwd=bad.cwd.join("other"), "namespace"=>bad.namespace=String::new(),
                "generation"=>bad.generation=0, "tokens"=>{}, _=>bad.deadline=Instant::now()-Duration::from_secs(1),
            }
            let r=if change=="tokens" { SamplingRequest::parse(json!({"maxTokens":33,"messages":req.messages()})).unwrap() } else { req.clone() };
            assert!(a.authorize(&bad,&r,&control).await.is_err(),"{change}");
        }
        a.authorize(&c,&req,&control).await.unwrap();
        let mut changed=c.clone(); changed.namespace="malicious".into();
        assert!(a.authorize(&changed,&req,&control).await.is_err());
        changed=c.clone(); changed.generation+=1;
        assert!(a.authorize(&changed,&req,&control).await.is_err());
        b.authorize(&context(&b),&req,&control).await.unwrap();
        assert!(a.authorize(&c,&req,&control).await.is_err());
        assert!(b.authorize(&context(&b),&req,&control).await.is_err());
    }
    #[tokio::test]
    async fn trusted_one_second_rule_shortens_three_second_parent_without_reset() {
        let mut value=file(false,2); value["resources"][0]["maxDurationSeconds"]=json!(1);
        let p=policy(value); let a=auth(&p,"root"); let mut c=context(&a);
        c.deadline=Instant::now()+Duration::from_secs(3);
        let before=Instant::now();
        let grant=a.authorize(&c,&request(),&McpControl::default()).await.unwrap();
        let after=Instant::now();
        assert_eq!(grant.max_duration,Duration::from_secs(1));
        assert!(grant.context.deadline>=before+Duration::from_secs(1));
        assert!(grant.context.deadline<=after+Duration::from_secs(1));
        assert!(grant.context.deadline<c.deadline);
        let mut expected=c.clone(); expected.deadline=grant.context.deadline;
        assert_eq!(grant.context,expected);
        // A shorter original deadline remains unchanged, never reset to now+rule.
        c.deadline=Instant::now()+Duration::from_millis(100);
        let grant=a.authorize(&c,&request(),&McpControl::default()).await.unwrap();
        assert_eq!(grant.context,c);
    }
    #[tokio::test]
    async fn execution_consumes_existing_one_second_grant_not_three_second_parent() {
        let mut value=file(false,1); value["resources"][0]["maxDurationSeconds"]=json!(1);
        let p=policy(value); let a=auth(&p,"root");
        let mut c=context(&a); c.deadline=Instant::now()+Duration::from_secs(3);
        let req=request(); let control=McpControl::default();
        let grant=a.authorize(&c,&req,&control).await.unwrap();
        // Delay after authorization must consume, not reset, the granted budget.
        let deadline=grant.context.deadline;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(grant.context.deadline,deadline);
        struct Pending;
        impl SamplingExecutor for Pending {
            fn execute<'a>(&'a self,_: &'a SamplingContext,_: &'a SamplingRequest,_: &'a McpControl)->SamplingFuture<'a> {
                Box::pin(std::future::pending())
            }
        }
        assert_eq!(execute_validated(Some(&Pending),Some(&grant),&c,&req,&control).await.unwrap_err(),SamplingError::TimedOut);
        assert!(Instant::now()<c.deadline);
    }
    #[tokio::test]
    async fn consent_cannot_outlive_granted_deadline() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut value=file(true,1); value["resources"][0]["maxDurationSeconds"]=json!(1);
        let p=policy(value); let mut a=auth(&p,"root");
        let (output,input)=tokio::io::duplex(8192);
        let writer=Arc::new(Writer::with_output(output));
        writer.negotiate_questions(&json!({"clientCapabilities":{"elicitation":{"form":{}}}}));
        a.writer=Arc::downgrade(&writer);
        let mut c=context(&a); c.deadline=Instant::now()+Duration::from_secs(3);
        let req=request(); let control=McpControl::default();
        let ui=async {
            let mut lines=BufReader::new(input).lines();
            let frame: Value=serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let cancel: Value=serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(cancel["method"],"$/cancel_request");
            writer.resolve(frame["id"].as_u64().unwrap(),json!({"action":"accept","content":{"approved":true}})).await;
        };
        let (grant,())=tokio::join!(a.authorize(&c,&req,&control),ui);
        assert!(grant.is_err());
        assert!(Instant::now()<c.deadline);
    }
    #[tokio::test]
    async fn consent_no_ui_consumes_attempt_and_never_grants() {
        let p=policy(file(true,1)); let a=auth(&p,"root");
        assert!(a.authorize(&context(&a),&request(),&McpControl::default()).await.is_err());
        assert_eq!(p.rules[0].remaining.load(Ordering::Acquire),0);
        assert!(preview(&SamplingRequest::parse(json!({"maxTokens":1,"messages":[{"role":"user","content":{"type":"text","text":"界".repeat(2000)}}]})).unwrap()).len()<=2048);
    }
    #[tokio::test]
    async fn no_optin_no_ui_and_exact_resource_cwd_gate_real_owner() {
        let cwd=std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let servers=crate::mcp::parse(&json!({"mcpServers":[{"type":"http","name":"configured","url":"https://mcp.example/mcp","headers":[]}]}),true,false).unwrap();
        let (host,_events)=HostSession::test_connection(tokio::io::sink(),16);
        let mut writer=Writer::with_output(tokio::io::sink()); let mut owner=None;
        assert!(options(&Arc::new(writer),&host,"root",&cwd,&servers,&mut owner).unwrap().is_none());
        writer=Writer::with_output(tokio::io::sink()); writer.mcp_sampling_policy=Ok(Some(policy(file(true,1))));
        assert!(options(&Arc::new(writer),&host,"root",&cwd,&servers,&mut owner).unwrap().is_none());
        for field in ["resource","cwd"] {
            let mut f=file(false,1);
            if field=="resource" { f["resources"][0][field]=json!("https://mcp.example/other"); }
            else { f["resources"][0][field]=json!(std::fs::canonicalize(cwd.parent().unwrap()).unwrap()); }
            writer=Writer::with_output(tokio::io::sink()); writer.mcp_sampling_policy=Ok(Some(policy(f)));
            assert!(options(&Arc::new(writer),&host,"root",&cwd,&servers,&mut owner).unwrap().is_none());
        }
        assert!(owner.is_none());
        writer=Writer::with_output(tokio::io::sink()); writer.mcp_sampling_policy=Ok(Some(policy(file(false,1))));
        assert!(options(&Arc::new(writer),&host,"root",&cwd,&servers,&mut owner).is_err()); // incapable native, no fake executor
        assert!(owner.is_none());
    }
}
