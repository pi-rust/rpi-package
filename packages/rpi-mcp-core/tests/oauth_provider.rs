//! Explicit HTTP loopback development fixtures; not TLS or external-account proof.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rpi_mcp_core::{oauth::*, *};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use url::Url;

#[derive(Default)]
struct State {
    token: AtomicUsize,
    refresh: AtomicUsize,
    authorized: AtomicUsize,
    posts: AtomicUsize,
    dcr: AtomicUsize,
    opens: AtomicUsize,
    bad: AtomicUsize,
    release: AtomicBool,
    transaction: Mutex<HashMap<String, String>>,
    originals: Mutex<Vec<Value>>,
}
struct Fixture {
    origin: String,
    state: Arc<State>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(mode: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let base = origin.clone();
        let state = Arc::new(State::default());
        let s = state.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let end = stop.clone();
        let worker = thread::spawn(move || {
            while !end.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                };
                // Windows accepted sockets inherit the listener's nonblocking
                // mode. The fixture intentionally uses blocking timed reads.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut input = Vec::new();
                let mut buf = [0; 4096];
                let (head, body) = loop {
                    let Ok(n) = stream.read(&mut buf) else {
                        break (String::new(), Vec::new());
                    };
                    if n == 0 {
                        break (String::new(), Vec::new());
                    }
                    input.extend_from_slice(&buf[..n]);
                    if let Some(i) = input.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&input[..i]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if input.len() >= i + 4 + len {
                            break (head, input[i + 4..i + 4 + len].to_vec());
                        }
                    }
                    assert!(input.len() < 65536);
                };
                if head.is_empty() {
                    continue;
                }
                let path = head.lines().next().unwrap().split(' ').nth(1).unwrap();
                let mut status = 200;
                let mut headers = String::new();
                let result = if path.starts_with("/.well-known/oauth-protected-resource") {
                    json!({"resource":format!("{base}/mcp{}", if mode==3 {"/wrong"} else {""}),"authorization_servers":[format!("{base}/issuer")],"bearer_methods_supported":["header"]})
                } else if path.starts_with("/.well-known/oauth-authorization-server/issuer") {
                    json!({"issuer":format!("{base}/{}", if mode==4 {"wrong"} else {"issuer"}),"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"registration_endpoint":format!("{base}/register"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"token_endpoint_auth_methods_supported":[if mode==21 {"client_secret_basic"} else {"none"}],"code_challenge_methods_supported":[if mode==20 {"plain"} else {"S256"}],"scopes_supported":["read"]})
                } else if path == "/register" {
                    s.dcr.fetch_add(1, Ordering::SeqCst);
                    status = 201;
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    assert!(
                        v["token_endpoint_auth_method"] == "none"
                            && v["grant_types"][0] == "authorization_code"
                    );
                    json!({"client_id":"fixture-public","redirect_uris":v["redirect_uris"],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]})
                } else if path.starts_with("/authorize?") {
                    s.opens.fetch_add(1, Ordering::SeqCst);
                    let u = Url::parse(&format!("{base}{path}")).unwrap();
                    let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
                    assert!(
                        q["resource"] == format!("{base}/mcp")
                            && q["scope"] == "read"
                            && q["code_challenge_method"] == "S256"
                            && q["client_id"] == "fixture-public"
                    );
                    assert!(q["state"].len() >= 43 && q["code_challenge"].len() == 43);
                    *s.transaction.lock().unwrap() = q.clone();
                    let mut redirect = Url::parse(&q["redirect_uri"]).unwrap();
                    redirect
                        .query_pairs_mut()
                        .append_pair("state", if mode == 1 { "wrong" } else { &q["state"] })
                        .append_pair("code", "fixture-code")
                        .append_pair("iss", &format!("{base}/issuer"));
                    status = 302;
                    headers = format!("Location: {}\r\n", redirect.as_str());
                    json!({})
                } else if path == "/token" {
                    s.token.fetch_add(1, Ordering::SeqCst);
                    if mode == 30 {
                        while !s.release.load(Ordering::SeqCst) && !end.load(Ordering::SeqCst) {
                            thread::sleep(Duration::from_millis(1));
                        }
                    }
                    let form: HashMap<String, String> =
                        url::form_urlencoded::parse(&body).into_owned().collect();
                    assert!(
                        form["resource"] == format!("{base}/mcp")
                            && form["client_id"] == "fixture-public"
                    );
                    if form["grant_type"] == "authorization_code" {
                        let q = s.transaction.lock().unwrap();
                        assert!(
                            form["redirect_uri"] == q["redirect_uri"]
                                && form["code"] == "fixture-code"
                        );
                        assert!(
                            URL_SAFE_NO_PAD
                                .encode(Sha256::digest(form["code_verifier"].as_bytes()))
                                == q["code_challenge"]
                        );
                    } else {
                        let count = s.refresh.fetch_add(1, Ordering::SeqCst);
                        assert!(form["scope"] == "read");
                        assert_eq!(
                            form["refresh_token"],
                            if count == 0 {
                                "refresh-one"
                            } else {
                                "refresh-two"
                            }
                        );
                        if mode == 6 {
                            status = 400;
                        }
                        if mode == 7 {
                            continue;
                        } // ambiguous response loss
                    }
                    if status == 400 {
                        json!({"error":"invalid_grant"})
                    } else {
                        let mut token = json!({"access_token":"provider-fixture-token","token_type":"Bearer","expires_in":120,"scope":if mode==2 {"wrong"} else {"read"},"refresh_token":if s.refresh.load(Ordering::SeqCst)>0 {"refresh-two"} else {"refresh-one"}});
                        match mode {
                            22 => token["scope"] = json!("read admin"),
                            23 => token["access_token"] = json!("bad\r\ntoken"),
                            24 => token["token_type"] = json!("Basic"),
                            25 => token["expires_in"] = json!(0),
                            26 => token["refresh_token"] = json!("bad\ntoken"),
                            _ => {}
                        }
                        token
                    }
                } else if path == "/mcp" {
                    s.posts.fetch_add(1, Ordering::SeqCst);
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    s.originals.lock().unwrap().push(v.clone());
                    if !head
                        .to_ascii_lowercase()
                        .contains("authorization: bearer provider-fixture-token")
                    {
                        status = 401;
                        // Existing HTTP challenge parser intentionally requires HTTPS explicit
                        // resource_metadata. Omitting it exercises standard PRM derivation
                        // under this provider's separately granted loopback development policy.
                        headers = "WWW-Authenticate: Bearer scope=\"read\"\r\n".into();
                        json!({})
                    } else {
                        s.authorized.fetch_add(1, Ordering::SeqCst);
                        json!({"jsonrpc":"2.0","id":v["id"],"result":{"ok":true}})
                    }
                } else {
                    s.bad.fetch_add(1, Ordering::SeqCst);
                    status = 404;
                    json!({})
                };
                if !path.starts_with("/mcp") {
                    assert!(!head.to_ascii_lowercase().contains("authorization:"));
                }
                let bytes = serde_json::to_vec(&result).unwrap();
                let response=format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",bytes.len());
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&bytes);
            }
        });
        Self {
            origin,
            state,
            stop,
            worker: Some(worker),
        }
    }
    fn binding(&self, generation: u64) -> AuthBinding {
        AuthBinding::new(
            123,
            "scope".into(),
            std::env::current_dir().unwrap(),
            "target".into(),
            format!("{}/mcp", self.origin),
            generation,
        )
        .unwrap()
    }
    fn provider(
        &self,
        dcr: bool,
        interaction: Arc<dyn AuthorizationInteraction>,
        max: usize,
    ) -> Arc<OAuthProvider> {
        let mut policy = NetworkPolicy::new(vec![]).unwrap();
        policy.allow_development_origin(&self.origin).unwrap();
        Arc::new(
            OAuthProvider::new(
                OAuthOptions {
                    resources: vec![ResourcePolicy {
                        resource: format!("{}/mcp", self.origin),
                        issuer: format!("{}/issuer", self.origin),
                        scopes: vec!["read".into()],
                        client_id: if dcr {
                            None
                        } else {
                            Some("fixture-public".into())
                        },
                        allow_dynamic_registration: dcr,
                    }],
                    max_bindings: max,
                    authorization_origins: vec![self.origin.clone()],
                },
                OAuthNetwork::new(policy).unwrap(),
                interaction,
            )
            .unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
struct Browser;
impl AuthorizationInteraction for Browser {
    fn authorize<'a>(&'a self, r: AuthorizationRequest<'a>) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            assert!(!format!("{r:?}").contains(r.authorization_url.as_str()));
            // Real authorize HTTP request and real callback, never fabricate a grant.
            let _ = reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(r.authorization_url)
                .send()
                .await;
            Ok(AuthorizationOutcome::Accepted)
        })
    }
}
struct Waiting {
    redirect: Arc<Mutex<Option<String>>>,
}
impl AuthorizationInteraction for Waiting {
    fn authorize<'a>(&'a self, r: AuthorizationRequest<'a>) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            *self.redirect.lock().unwrap() = r
                .authorization_url
                .query_pairs()
                .find(|(k, _)| k == "redirect_uri")
                .map(|(_, v)| v.into_owned());
            Ok(AuthorizationOutcome::Accepted)
        })
    }
}
fn challenge() -> BearerChallenge {
    BearerChallenge {
        resource_metadata: None,
        scope: Some("read".into()),
        error: None,
    }
}
async fn get(
    p: &OAuthProvider,
    b: &AuthBinding,
    reason: AuthReason<'_>,
    c: &Control,
) -> Result<AuthDecision, AuthError> {
    p.credential(AuthRequest {
        binding: b,
        reason,
        control: c,
        deadline: Instant::now() + Duration::from_secs(4),
    })
    .await
}
fn lease(d: AuthDecision) -> CredentialLease {
    match d {
        AuthDecision::Credential(l) => l,
        _ => panic!("expected credential"),
    }
}

#[test]
fn real_401_discovery_dcr_authorize_pkce_token_and_identical_resend() {
    for dcr in [false, true] {
        let f = Fixture::new(0);
        let p = f.provider(dcr, Arc::new(Browser), 8);
        let server: HttpServer =
            serde_json::from_value(json!({"url":format!("{}/mcp",f.origin),"timeout":5})).unwrap();
        let mut transport =
            rpi_mcp_core::http::HttpTransport::with_auth(&server, f.binding(1), p).unwrap();
        let original = json!({"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"test","arguments":{}}});
        assert!(transport.raw(original.clone(), &Control::default()).is_ok());
        assert_eq!(f.state.posts.load(Ordering::SeqCst), 2);
        assert_eq!(f.state.authorized.load(Ordering::SeqCst), 1);
        assert_eq!(f.state.token.load(Ordering::SeqCst), 1);
        assert_eq!(f.state.dcr.load(Ordering::SeqCst), usize::from(dcr));
        assert!(f
            .state
            .originals
            .lock()
            .unwrap()
            .iter()
            .all(|v| v == &original));
        assert!(transport.raw(original, &Control::default()).is_ok());
        assert_eq!(f.state.token.load(Ordering::SeqCst), 1);
        assert_eq!(f.state.bad.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn invalid_state_scope_resource_and_issuer_never_commit() {
    for mode in [1, 2, 3, 4, 20, 21, 22, 23, 24, 25, 26] {
        let f = Fixture::new(mode);
        let p = f.provider(false, Arc::new(Browser), 8);
        let b = f.binding(1);
        let c = Control::default();
        let ch = challenge();
        let result = get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: None,
            },
            &c,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            f.state.token.load(Ordering::SeqCst),
            usize::from(mode == 2 || mode >= 22)
        );
        assert!(matches!(
            get(&p, &b, AuthReason::CachedOnly, &c).await.unwrap(),
            AuthDecision::Unavailable
        ));
    }
}
#[tokio::test]
async fn refresh_singleflight_exact_rejection_and_full_binding_capacity() {
    let f = Fixture::new(0);
    let p = f.provider(false, Arc::new(Browser), 1);
    let b = f.binding(1);
    let c = Control::default();
    let ch = challenge();
    assert!(matches!(
        get(&p, &b, AuthReason::CachedOnly, &c).await.unwrap(),
        AuthDecision::Unavailable
    ));
    assert_eq!(f.state.opens.load(Ordering::SeqCst), 0);
    let first = lease(
        get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: None,
            },
            &c,
        )
        .await
        .unwrap(),
    );
    p.rejected_lease(&b, first.id());
    let (a, z) = tokio::join!(
        get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: Some(first.id())
            },
            &c
        ),
        get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: Some(first.id())
            },
            &c
        )
    );
    let a = lease(a.unwrap());
    let z = lease(z.unwrap());
    assert!(a.id() == z.id() && a.id() != first.id());
    assert_eq!(f.state.refresh.load(Ordering::SeqCst), 1);
    p.rejected_lease(&b, first.id());
    assert_eq!(
        lease(get(&p, &b, AuthReason::CachedOnly, &c).await.unwrap()).id(),
        a.id()
    );
    assert_eq!(
        get(&p, &f.binding(2), AuthReason::CachedOnly, &c)
            .await
            .unwrap_err(),
        AuthError::Provider
    );
}
#[tokio::test]
async fn invalid_grant_reauth_once_but_ambiguous_refresh_stops() {
    for mode in [6, 7] {
        let f = Fixture::new(mode);
        let p = f.provider(false, Arc::new(Browser), 8);
        let b = f.binding(1);
        let c = Control::default();
        let ch = challenge();
        let first = lease(
            get(
                &p,
                &b,
                AuthReason::Challenge {
                    challenge: &ch,
                    rejected_lease: None,
                },
                &c,
            )
            .await
            .unwrap(),
        );
        let result = get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: Some(first.id()),
            },
            &c,
        )
        .await;
        if mode == 6 {
            assert!(matches!(result, Ok(AuthDecision::Credential(_))));
            assert_eq!(f.state.opens.load(Ordering::SeqCst), 2);
        } else {
            assert!(result.is_err());
            assert_eq!(f.state.opens.load(Ordering::SeqCst), 1);
            assert!(matches!(
                get(&p, &b, AuthReason::CachedOnly, &c).await.unwrap(),
                AuthDecision::Unavailable
            ));
        }
        assert_eq!(f.state.refresh.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn cancellation_and_timeout_close_callback_no_late_commit() {
    for cancel in [true, false] {
        let f = Fixture::new(0);
        let redirect = Arc::new(Mutex::new(None));
        let p = f.provider(
            false,
            Arc::new(Waiting {
                redirect: redirect.clone(),
            }),
            8,
        );
        let b = f.binding(1);
        let c = Control::default();
        let ch = challenge();
        let wait = p.credential(AuthRequest {
            binding: &b,
            reason: AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: None,
            },
            control: &c,
            deadline: Instant::now() + Duration::from_secs(2),
        });
        let signal = async {
            let end = Instant::now() + Duration::from_secs(2);
            while redirect.lock().unwrap().is_none() && Instant::now() < end {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            if cancel {
                c.cancel();
            }
        };
        let (result, _) = tokio::join!(wait, signal);
        assert_eq!(
            result.unwrap_err(),
            if cancel {
                AuthError::Cancelled
            } else {
                AuthError::Timeout
            }
        );
        let address = Url::parse(redirect.lock().unwrap().as_ref().unwrap()).unwrap();
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", address.port().unwrap()))
                .await
                .is_err()
        );
        assert_eq!(f.state.token.load(Ordering::SeqCst), 0);
        assert!(matches!(
            get(&p, &b, AuthReason::CachedOnly, &Control::default())
                .await
                .unwrap(),
            AuthDecision::Unavailable
        ));
    }
}

#[test]
fn refresh_gate_is_shared_across_distinct_private_runtimes() {
    let f = Fixture::new(0);
    let p = f.provider(false, Arc::new(Browser), 8);
    let b = f.binding(1);
    let first = tokio::runtime::Runtime::new().unwrap().block_on(async {
        lease(
            get(
                &p,
                &b,
                AuthReason::Challenge {
                    challenge: &challenge(),
                    rejected_lease: None,
                },
                &Control::default(),
            )
            .await
            .unwrap(),
        )
    });
    p.rejected_lease(&b, first.id());
    let mut workers = Vec::new();
    for _ in 0..4 {
        let p = p.clone();
        let b = b.clone();
        let id = first.id().to_owned();
        workers.push(thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    lease(
                        get(
                            &p,
                            &b,
                            AuthReason::Challenge {
                                challenge: &challenge(),
                                rejected_lease: Some(&id),
                            },
                            &Control::default(),
                        )
                        .await
                        .unwrap(),
                    )
                    .id()
                    .to_owned()
                })
        }));
    }
    let ids: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(f.state.refresh.load(Ordering::SeqCst), 1);
}

struct RawCallback(&'static str);
impl AuthorizationInteraction for RawCallback {
    fn authorize<'a>(&'a self, r: AuthorizationRequest<'a>) -> AuthorizationFuture<'a> {
        Box::pin(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let q: HashMap<String, String> =
                r.authorization_url.query_pairs().into_owned().collect();
            let redirect = Url::parse(&q["redirect_uri"]).unwrap();
            let host = format!("127.0.0.1:{}", redirect.port().unwrap());
            let mut target = format!("/oauth/callback?state={}&code=fixture-code", q["state"]);
            let mut header = format!("Host: {host}\r\n");
            let mut body = "";
            match self.0 {
                "duplicate" => target.push_str("&state=duplicate"),
                "missing" => target = "/oauth/callback?code=fixture-code".into(),
                "issuer" => target.push_str("&iss=https%3A%2F%2Fwrong.example"),
                "path" => target = target.replace("/oauth/callback?", "/wrong?"),
                "host" => header = "Host: wrong.example\r\n".into(),
                "deny" => target.push_str("&error=access_denied"),
                "fragment" => target.push_str("#fragment"),
                "control" => target.push_str("%0d%0a"),
                "body" => {
                    header.push_str("Content-Length: 1\r\n");
                    body = "x";
                }
                "chunked" => header.push_str("Transfer-Encoding: chunked\r\n"),
                _ => panic!("unknown callback case"),
            }
            let mut stream = tokio::net::TcpStream::connect(&host).await.unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\n{header}\r\n{body}").as_bytes())
                .await
                .unwrap();
            let mut response = Vec::new();
            let _ = stream.read_to_end(&mut response).await;
            Ok(AuthorizationOutcome::Accepted)
        })
    }
}

#[tokio::test]
async fn actual_callback_invalid_requests_fail_closed_without_exchange() {
    for case in [
        "duplicate",
        "missing",
        "issuer",
        "path",
        "host",
        "deny",
        "fragment",
        "control",
        "body",
        "chunked",
    ] {
        let f = Fixture::new(0);
        let p = f.provider(false, Arc::new(RawCallback(case)), 1);
        let b = f.binding(1);
        let result = get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &challenge(),
                rejected_lease: None,
            },
            &Control::default(),
        )
        .await;
        assert_eq!(result.unwrap_err(), AuthError::InvalidState, "{case}");
        assert_eq!(f.state.token.load(Ordering::SeqCst), 0, "{case}");
        assert!(matches!(
            get(&p, &b, AuthReason::CachedOnly, &Control::default())
                .await
                .unwrap(),
            AuthDecision::Unavailable
        ));
    }
}

#[tokio::test]
async fn forget_and_clear_cancel_slot_generations_and_release_capacity() {
    for clear in [false, true] {
        let f = Fixture::new(0);
        let redirect = Arc::new(Mutex::new(None));
        let p = f.provider(
            false,
            Arc::new(Waiting {
                redirect: redirect.clone(),
            }),
            1,
        );
        let b = f.binding(1);
        let c = Control::default();
        let ch = challenge();
        let pending = get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: None,
            },
            &c,
        );
        let retire = async {
            while redirect.lock().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            if clear {
                p.clear().unwrap();
            } else {
                assert!(p.forget_binding(&b).unwrap());
            }
            // A fresh slot with the SAME binding must not be poisoned by the old future.
            assert!(matches!(
                get(&p, &b, AuthReason::CachedOnly, &c).await.unwrap(),
                AuthDecision::Unavailable
            ));
        };
        let (result, _) = tokio::join!(pending, retire);
        assert_eq!(result.unwrap_err(), AuthError::Cancelled);
        let address = Url::parse(redirect.lock().unwrap().as_ref().unwrap()).unwrap();
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", address.port().unwrap()))
                .await
                .is_err()
        );
        assert!(p.forget_binding(&b).unwrap());
        assert!(!p.forget_binding(&b).unwrap());
        assert!(matches!(
            get(&p, &f.binding(2), AuthReason::CachedOnly, &c)
                .await
                .unwrap(),
            AuthDecision::Unavailable
        ));
        assert_eq!(f.state.token.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn pending_exchange_cancellation_or_forget_cannot_commit() {
    for forget in [false, true] {
        let f = Fixture::new(30);
        let p = f.provider(false, Arc::new(Browser), 1);
        let b = f.binding(1);
        let c = Control::default();
        let ch = challenge();
        let pending = get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: None,
            },
            &c,
        );
        let cancel = async {
            while f.state.token.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            if forget {
                p.forget_binding(&b).unwrap();
            } else {
                c.cancel();
            }
        };
        let (result, _) = tokio::join!(pending, cancel);
        assert_eq!(result.unwrap_err(), AuthError::Cancelled);
        f.state.release.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(matches!(
            get(&p, &b, AuthReason::CachedOnly, &Control::default())
                .await
                .unwrap(),
            AuthDecision::Unavailable
        ));
    }
}

#[tokio::test]
async fn refresh_rotation_retains_new_token_for_next_rejection() {
    let f = Fixture::new(0);
    let p = f.provider(false, Arc::new(Browser), 1);
    let b = f.binding(1);
    let c = Control::default();
    let ch = challenge();
    let mut current = lease(
        get(
            &p,
            &b,
            AuthReason::Challenge {
                challenge: &ch,
                rejected_lease: None,
            },
            &c,
        )
        .await
        .unwrap(),
    );
    for _ in 0..2 {
        current = lease(
            get(
                &p,
                &b,
                AuthReason::Challenge {
                    challenge: &ch,
                    rejected_lease: Some(current.id()),
                },
                &c,
            )
            .await
            .unwrap(),
        );
    }
    assert_eq!(f.state.refresh.load(Ordering::SeqCst), 2);
    assert_eq!(f.state.opens.load(Ordering::SeqCst), 1);
    assert_eq!(
        lease(get(&p, &b, AuthReason::CachedOnly, &c).await.unwrap()).id(),
        current.id()
    );
}

struct Decline;
impl AuthorizationInteraction for Decline {
    fn authorize<'a>(&'a self, _: AuthorizationRequest<'a>) -> AuthorizationFuture<'a> {
        Box::pin(async { Ok(AuthorizationOutcome::Declined) })
    }
}

#[tokio::test]
async fn trusted_dcr_approval_is_required_and_browser_decline_never_exchanges() {
    let f = Fixture::new(0);
    let mut policy = NetworkPolicy::new(vec![]).unwrap();
    policy.allow_development_origin(&f.origin).unwrap();
    let denied = OAuthProvider::new(
        OAuthOptions {
            resources: vec![ResourcePolicy {
                resource: format!("{}/mcp", f.origin),
                issuer: format!("{}/issuer", f.origin),
                scopes: vec!["read".into()],
                client_id: None,
                allow_dynamic_registration: false,
            }],
            max_bindings: 1,
            authorization_origins: vec![f.origin.clone()],
        },
        OAuthNetwork::new(policy).unwrap(),
        Arc::new(Decline),
    );
    assert_eq!(denied.unwrap_err(), AuthError::Registration);
    assert_eq!(f.state.dcr.load(Ordering::SeqCst), 0);
    for dcr in [false, true] {
        let p = f.provider(dcr, Arc::new(Decline), 1);
        let b = f.binding(1);
        assert!(matches!(
            get(
                &p,
                &b,
                AuthReason::Challenge {
                    challenge: &challenge(),
                    rejected_lease: None
                },
                &Control::default()
            )
            .await
            .unwrap(),
            AuthDecision::Declined
        ));
        assert!(matches!(
            get(&p, &b, AuthReason::CachedOnly, &Control::default())
                .await
                .unwrap(),
            AuthDecision::Unavailable
        ));
        // Explicit trusted configuration authorizes registration BEFORE browser
        // consent. A browser decline does not undo that separately approved DCR.
        assert_eq!(f.state.dcr.load(Ordering::SeqCst), usize::from(dcr));
        assert_eq!(f.state.token.load(Ordering::SeqCst), 0);
    }
}
