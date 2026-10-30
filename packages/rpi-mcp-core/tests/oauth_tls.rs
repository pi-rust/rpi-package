//! Real sockets and TLS; no certificate-verification bypass or external DNS.
use rpi_mcp_core::{
    oauth::network::{NetworkPolicy, OAuthNetwork, Resolver},
    AuthError, Control,
};
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::{rustls, TlsAcceptor};
use url::Url;

const CA: &[u8] = include_bytes!("testcertfixtures/ca.pem");
const CERT: &[u8] = include_bytes!("testcertfixtures/server.pem");
// Deliberately public TEST ONLY key; never use this CA or key outside tests.
const KEY: &[u8] = include_bytes!("testcertfixtures/server-key.pem");
fn end() -> Instant {
    Instant::now() + Duration::from_secs(4)
}
struct Server {
    port: u16,
    hits: Arc<AtomicUsize>,
    accepts: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(tls: bool, location: Option<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let accepts = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let a = accepts.clone();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                rustls::pki_types::CertificateDer::pem_slice_iter(CERT)
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                rustls::pki_types::PrivateKeyDer::from_pem_slice(KEY).unwrap(),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                a.fetch_add(1, Ordering::SeqCst);
                // A child connection is bounded and owned by this server task:
                // no detached connection tasks survive fixture teardown.
                let work = async {
                    if tls {
                        if let Ok(mut stream) = acceptor.accept(socket).await {
                            serve(&mut stream, &h, location.as_deref()).await;
                        }
                    } else {
                        let mut stream = socket;
                        serve(&mut stream, &h, location.as_deref()).await;
                    }
                };
                let _ = tokio::time::timeout(Duration::from_secs(3), work).await;
            }
        });
        Self {
            port,
            hits,
            accepts,
            task,
        }
    }
    fn origin(&self, host: &str) -> String {
        format!("https://{host}:{}", self.port)
    }
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }
}
use rustls::pki_types::pem::PemObject;
async fn serve<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    hits: &AtomicUsize,
    location: Option<&str>,
) {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0; 1024];
        let Ok(n) = stream.read(&mut buf).await else {
            return;
        };
        if n == 0 {
            return;
        }
        bytes.extend_from_slice(&buf[..n]);
        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > 8192 {
            return;
        }
    }
    hits.fetch_add(1, Ordering::SeqCst);
    let body = "{\"tls\":true}";
    let head = match location {
        Some(url) => format!("HTTP/1.1 302 Found\r\nLocation: {url}\r\n"),
        None => "HTTP/1.1 200 OK\r\n".into(),
    };
    let _ = stream.write_all(format!("{head}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
    let _ = stream.shutdown().await;
}
struct ChangingResolver {
    calls: AtomicUsize,
    answers: Vec<Vec<SocketAddr>>,
}
impl ChangingResolver {
    fn new(answers: Vec<Vec<SocketAddr>>) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            answers,
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
impl Resolver for ChangingResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, AuthError>> + Send + 'a>> {
        Box::pin(async move {
            assert!(host.ends_with(".invalid"));
            assert_ne!(port, 0);
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.answers[index.min(self.answers.len() - 1)].clone())
        })
    }
}
fn network(origin: &str, resolver: Option<Arc<dyn Resolver>>, trust: bool) -> OAuthNetwork {
    let mut policy = NetworkPolicy::new(vec![]).unwrap();
    policy
        .allow_private_origin(origin, vec!["127.0.0.1".parse().unwrap()])
        .unwrap();
    let mut net = OAuthNetwork::new(policy).unwrap();
    if trust {
        net = net.with_root_certificates(vec![reqwest::Certificate::from_pem(CA).unwrap()]);
    }
    if let Some(resolver) = resolver {
        net = net.with_resolver(resolver);
    }
    net
}
#[tokio::test]
async fn real_https_trusted_ca_and_ip_san_but_untrusted_and_wrong_hostname_fail() {
    let server = Server::start(true, None).await;
    let origin = server.origin("127.0.0.1");
    let url = Url::parse(&origin).unwrap();
    assert_eq!(
        network(&origin, None, true)
            .get_json(&url, &Control::default(), end())
            .await
            .unwrap()
            .json["tls"],
        true
    );
    assert_eq!(
        network(&origin, None, false)
            .get_json(&url, &Control::default(), end())
            .await
            .unwrap_err(),
        AuthError::Exchange
    );
    let wrong = server.origin("wrong.example.invalid");
    let resolver = ChangingResolver::new(vec![vec![format!("127.0.0.1:{}", server.port)
        .parse()
        .unwrap()]]);
    assert_eq!(
        network(&wrong, Some(resolver.clone()), true)
            .get_json(&Url::parse(&wrong).unwrap(), &Control::default(), end())
            .await
            .unwrap_err(),
        AuthError::Exchange
    );
    assert_eq!(resolver.calls(), 1);
    assert_eq!(server.hits(), 1);
    assert_eq!(server.accepts(), 3);
}
#[tokio::test]
async fn real_hostname_tls_pins_checked_dns_and_resolves_fresh_per_request() {
    let server = Server::start(true, None).await;
    let origin = server.origin("oauth.example.invalid");
    let resolver = ChangingResolver::new(vec![
        vec![format!("127.0.0.1:{}", server.port).parse().unwrap()],
        vec!["127.0.0.2:1".parse().unwrap()],
    ]);
    let net = network(&origin, Some(resolver.clone()), true);
    let url = Url::parse(&origin).unwrap();
    assert!(net.validate_resource_url(&url).is_ok());
    assert_eq!(resolver.calls(), 0); // validation is strictly side-effect-free
    assert!(net.get_json(&url, &Control::default(), end()).await.is_ok());
    assert_eq!(resolver.calls(), 1); // no second lookup inside actual TLS connect
    assert_eq!(server.hits(), 1);
    assert_eq!(
        net.get_json(&url, &Control::default(), end())
            .await
            .unwrap_err(),
        AuthError::UnsafeUrl
    );
    assert_eq!(resolver.calls(), 2); // no cross-request DNS/connection cache
    assert_eq!(server.accepts(), 1);
}
#[tokio::test]
async fn real_connector_rejects_all_bad_or_mixed_answers_before_connect() {
    let server = Server::start(true, None).await;
    let origin = server.origin("oauth.example.invalid");
    let good: SocketAddr = format!("127.0.0.1:{}", server.port).parse().unwrap();
    for addresses in [
        vec![],
        vec![good, "8.8.8.8:443".parse().unwrap()],
        vec![good, "10.0.0.1:443".parse().unwrap()],
        vec![good, "[::ffff:127.0.0.1]:443".parse().unwrap()],
    ] {
        let resolver = ChangingResolver::new(vec![addresses]);
        assert_eq!(
            network(&origin, Some(resolver.clone()), true)
                .get_json(&Url::parse(&origin).unwrap(), &Control::default(), end())
                .await
                .unwrap_err(),
            AuthError::UnsafeUrl
        );
        assert_eq!(resolver.calls(), 1);
    }
    // Without a private grant, even the otherwise-valid loopback TLS server is forbidden.
    let public = "https://oauth.example.com";
    struct PublicResolver(SocketAddr);
    impl Resolver for PublicResolver {
        fn resolve<'a>(
            &'a self,
            _: &'a str,
            _: u16,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, AuthError>> + Send + 'a>> {
            Box::pin(async move { Ok(vec!["8.8.8.8:443".parse().unwrap(), self.0]) })
        }
    }
    let net = OAuthNetwork::new(NetworkPolicy::new(vec![public.into()]).unwrap())
        .unwrap()
        .with_resolver(Arc::new(PublicResolver(good)));
    assert_eq!(
        net.get_json(&Url::parse(public).unwrap(), &Control::default(), end())
            .await
            .unwrap_err(),
        AuthError::UnsafeUrl
    );
    tokio::task::yield_now().await;
    assert_eq!(server.accepts(), 0);
}
#[tokio::test]
async fn real_https_redirect_and_http_downgrade_never_connect_to_targets() {
    for tls in [true, false] {
        let target = Server::start(tls, None).await;
        let target_url = format!(
            "{}://127.0.0.1:{}/target",
            if tls { "https" } else { "http" },
            target.port
        );
        let source = Server::start(true, Some(target_url)).await;
        let origin = source.origin("127.0.0.1");
        let mut policy = NetworkPolicy::new(vec![]).unwrap();
        policy
            .allow_private_origin(&origin, vec!["127.0.0.1".parse().unwrap()])
            .unwrap();
        if tls {
            policy
                .allow_private_origin(
                    &target.origin("127.0.0.1"),
                    vec!["127.0.0.1".parse().unwrap()],
                )
                .unwrap();
        } else {
            policy
                .allow_development_origin(&format!("http://127.0.0.1:{}", target.port))
                .unwrap();
        }
        let net = OAuthNetwork::new(policy)
            .unwrap()
            .with_root_certificates(vec![reqwest::Certificate::from_pem(CA).unwrap()]);
        assert_eq!(
            net.get_json(&Url::parse(&origin).unwrap(), &Control::default(), end())
                .await
                .unwrap_err(),
            AuthError::Exchange
        );
        assert_eq!(source.hits(), 1);
        assert_eq!(target.hits(), 0);
        assert_eq!(target.accepts(), 0);
    }
}

#[test]
fn proxy_environment_is_ignored_in_isolated_subprocess() {
    if std::env::var_os("OAUTH_PROXY_CHILD").is_some() {
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "proxy_child_real_tls", "--nocapture"])
        .env("OAUTH_PROXY_CHILD", "1")
        .status()
        .unwrap();
    assert!(status.success());
}
#[tokio::test]
async fn proxy_child_real_tls() {
    if std::env::var_os("OAUTH_PROXY_CHILD").is_none() {
        return;
    }
    let proxy = Server::start(false, None).await;
    let origin = Server::start(true, None).await;
    let poison = format!("http://127.0.0.1:{}", proxy.port);
    // Only this isolated test process changes environment, before constructing clients.
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        std::env::set_var(key, &poison);
    }
    for key in ["NO_PROXY", "no_proxy"] {
        std::env::remove_var(key);
    }
    let url = origin.origin("oauth.example.invalid");
    let resolver = ChangingResolver::new(vec![vec![format!("127.0.0.1:{}", origin.port)
        .parse()
        .unwrap()]]);
    assert!(network(&url, Some(resolver.clone()), true)
        .get_json(&Url::parse(&url).unwrap(), &Control::default(), end())
        .await
        .is_ok());
    assert_eq!(origin.hits(), 1);
    assert_eq!(resolver.calls(), 1);
    assert_eq!(proxy.accepts(), 0);
    assert_eq!(proxy.hits(), 0);
    println!("isolated poisoned HTTP_PROXY/HTTPS_PROXY/ALL_PROXY: TLS origin hits=1, proxy TCP accepts=0, proxy HTTP hits=0");
}
