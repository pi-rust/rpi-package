//! Downloaded native providers are staged in immutable, content-addressed files.
//! A separate selection record is changed only after complete verification.
use futures_util::StreamExt;
use rpi_components::{Bundle, NativeArtifact};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Selection {
    pub id: String,
    pub version: String,
    pub bundle_sha256: String,
    pub artifact_sha256: String,
    pub target: String,
    pub entry_flag: String,
}
pub struct Provider {
    pub library: PathBuf,
    pub selection: Selection,
}
pub struct Store {
    root: PathBuf,
}
pub fn target() -> Result<&'static str, String> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "windows") => Ok("x86_64-pc-windows-msvc"),
        ("aarch64", "windows") => Ok("aarch64-pc-windows-msvc"),
        ("x86_64", "linux") => Ok("x86_64-unknown-linux-gnu"),
        ("aarch64", "linux") => Ok("aarch64-unknown-linux-gnu"),
        ("x86_64", "macos") => Ok("x86_64-apple-darwin"),
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        _ => Err("This platform has no supported native node provider".into()),
    }
}
impl Store {
    /// Select the application default on next startup. Immutable files may
    /// still be loaded by the running process and are deliberately retained.
    pub fn clear_selection(&self) -> Result<(), String> {
        match std::fs::remove_file(self.root.join("selection.json")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Cannot restore default node provider".into()),
        }
    }
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn selected(&self) -> Result<Option<Provider>, String> {
        let file = self.root.join("selection.json");
        if !file.exists() {
            return Ok(None);
        }
        let metadata = std::fs::symlink_metadata(&file)
            .map_err(|_| "Cannot read native provider selection")?;
        if !metadata.is_file() || metadata.len() > 16 * 1024 {
            return Err("Invalid native provider selection".into());
        }
        let s: Selection = serde_json::from_slice(
            &std::fs::read(file).map_err(|_| "Cannot read provider selection")?,
        )
        .map_err(|_| "Invalid provider selection")?;
        if !rpi_components::valid_id(&s.id)
            || s.target != target()?
            || !hex_digest(&s.bundle_sha256)
            || !hex_digest(&s.artifact_sha256)
        {
            return Err("Invalid provider selection identity".into());
        }
        let directory = self.root.join(&s.id).join(&s.bundle_sha256);
        let root = self
            .root
            .canonicalize()
            .map_err(|_| "Native provider store is missing")?;
        if !directory
            .canonicalize()
            .map_err(|_| "Native provider directory is missing")?
            .starts_with(&root)
        {
            return Err("Native provider directory escaped store".into());
        }
        let metadata = std::fs::symlink_metadata(directory.join("component.json"))
            .map_err(|_| "Native provider bundle is missing")?;
        if !metadata.is_file() || metadata.len() > rpi_components::MAX_BUNDLE as u64 {
            return Err("Invalid native provider bundle size".into());
        }
        let bundle: Bundle = serde_json::from_slice(
            &std::fs::read(directory.join("component.json"))
                .map_err(|_| "Native provider bundle is missing")?,
        )
        .map_err(|_| "Invalid native provider bundle")?;
        bundle.validate()?;
        let runtime = bundle
            .manifest
            .runtime
            .as_ref()
            .ok_or("Native provider descriptor is missing")?;
        let artifact = runtime
            .artifacts
            .get(&s.target)
            .ok_or("Native platform is unavailable")?;
        if bundle.manifest.id != s.id
            || bundle.manifest.version != s.version
            || bundle.sha256 != s.bundle_sha256
            || artifact.sha256 != s.artifact_sha256
            || runtime.entry_flag != s.entry_flag
        {
            return Err("Native provider selection mismatch".into());
        }
        let library = directory.join(format!("runtime{}", std::env::consts::DLL_SUFFIX));
        verify_file(&library, artifact)?;
        Ok(Some(Provider {
            library,
            selection: s,
        }))
    }
    pub async fn install(
        &self,
        bundle: Bundle,
        allow_native_code: bool,
    ) -> Result<Selection, String> {
        bundle.validate()?;
        if !allow_native_code {
            return Err("Explicit native code trust is required".into());
        }
        let runtime = bundle
            .manifest
            .runtime
            .as_ref()
            .ok_or("Component contains no native provider")?;
        let target = target()?;
        runtime
            .artifacts
            .get(target)
            .ok_or("No native artifact for this platform")?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let url = attempt.url();
                if attempt.previous().len() >= 5
                    || url.scheme() != "https"
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    attempt.error("Unsafe native download redirect")
                } else {
                    attempt.follow()
                }
            }))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|_| "Cannot create native download transport")?;
        self.download(bundle, client).await
    }
    async fn download(&self, bundle: Bundle, client: reqwest::Client) -> Result<Selection, String> {
        bundle.validate()?;
        let runtime = bundle
            .manifest
            .runtime
            .as_ref()
            .ok_or("Component contains no native provider")?;
        let artifact = runtime
            .artifacts
            .get(target()?)
            .ok_or("No native artifact for this platform")?;
        // This transport has no cookies or node/password/capability headers.
        let response = client
            .get(&artifact.url)
            .send()
            .await
            .map_err(|_| "Native artifact download failed")?;
        self.stage_response(bundle, response).await
    }
    async fn stage_response(
        &self,
        bundle: Bundle,
        response: reqwest::Response,
    ) -> Result<Selection, String> {
        let runtime = bundle
            .manifest
            .runtime
            .as_ref()
            .ok_or("Component contains no native provider")?;
        let target = target()?;
        let artifact = runtime
            .artifacts
            .get(target)
            .ok_or("No native artifact for this platform")?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|n| n != artifact.bytes)
        {
            return Err("Native artifact response is invalid".into());
        }
        let directory = self.root.join(&bundle.manifest.id).join(&bundle.sha256);
        std::fs::create_dir_all(&directory)
            .map_err(|_| "Cannot prepare native provider directory")?;
        let root = self
            .root
            .canonicalize()
            .map_err(|_| "Cannot resolve native provider directory")?;
        if !directory
            .canonicalize()
            .map_err(|_| "Cannot resolve native provider directory")?
            .starts_with(&root)
        {
            return Err("Native provider directory escaped store".into());
        }
        let mut temp = tempfile::NamedTempFile::new_in(&directory)
            .map_err(|_| "Cannot stage native provider")?;
        let mut digest = Sha256::new();
        let mut length = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "Native artifact transfer was interrupted")?;
            length = length
                .checked_add(chunk.len() as u64)
                .filter(|n| *n <= artifact.bytes)
                .ok_or("Native artifact exceeds declared size")?;
            digest.update(&chunk);
            temp.write_all(&chunk)
                .map_err(|_| "Cannot write native provider")?;
        }
        if length != artifact.bytes || format!("{:x}", digest.finalize()) != artifact.sha256 {
            return Err("Native artifact digest or size mismatch".into());
        }
        temp.as_file()
            .sync_all()
            .map_err(|_| "Cannot save native provider")?;
        let library = directory.join(format!("runtime{}", std::env::consts::DLL_SUFFIX));
        if library.exists() {
            verify_file(&library, artifact)?;
        } else {
            temp.persist_noclobber(&library)
                .map_err(|_| "Cannot publish native provider")?;
        }
        write_json(&directory.join("component.json"), &bundle)?;
        let selected = Selection {
            id: bundle.manifest.id.clone(),
            version: bundle.manifest.version.clone(),
            bundle_sha256: bundle.sha256.clone(),
            artifact_sha256: artifact.sha256.clone(),
            target: target.into(),
            entry_flag: runtime.entry_flag.clone(),
        };
        write_json(&self.root.join("selection.json"), &selected)?;
        Ok(selected)
    }
}
fn hex_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn verify_file(path: &Path, artifact: &NativeArtifact) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "Native provider file is missing")?;
    if !metadata.is_file() || metadata.len() != artifact.bytes {
        return Err("Native provider file size mismatch".into());
    }
    let mut file = std::fs::File::open(path).map_err(|_| "Cannot read native provider")?;
    let mut digest = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|_| "Cannot verify native provider")?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    if format!("{:x}", digest.finalize()) != artifact.sha256 {
        return Err("Native provider digest mismatch".into());
    }
    Ok(())
}
fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().ok_or("Invalid provider path")?)
        .map_err(|_| "Cannot stage native provider metadata")?;
    file.write_all(
        &serde_json::to_vec(value).map_err(|_| "Cannot encode native provider metadata")?,
    )
    .and_then(|()| file.as_file().sync_all())
    .map_err(|_| "Cannot save native provider metadata")?;
    file.persist(path)
        .map_err(|_| "Cannot publish native provider metadata")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpi_components::{Manifest, NativeRuntime, Panel};
    use std::{collections::BTreeMap, net::TcpListener};
    async fn tls_fixture(bytes: Vec<u8>) -> (String, reqwest::Client, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_rustls::{
            rustls::{
                self,
                pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
            },
            TlsAcceptor,
        };
        let certificate = include_bytes!("../tests/fixtures/localhost-cert.der");
        let key = include_bytes!("../tests/fixtures/localhost-key.der");
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = TlsAcceptor::from(std::sync::Arc::new(config))
                .accept(socket)
                .await
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8];
            while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                assert_ne!(socket.read(&mut byte).await.unwrap(), 0);
                request.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(
                !request.contains("authorization:")
                    && !request.contains("cookie:")
                    && !request.contains("x-rpi-node-control:")
            );
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&bytes).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .add_root_certificate(reqwest::Certificate::from_der(certificate).unwrap())
            .build()
            .unwrap();
        (
            format!("https://localhost:{}/runtime", address.port()),
            client,
            server,
        )
    }
    #[tokio::test]
    async fn native_https_transfer_validates_the_bundle_and_artifact() {
        let bytes = b"isolated-native-transfer";
        let (url, client, server) = tls_fixture(bytes.to_vec()).await;
        let mut bundle = package(bytes);
        bundle
            .manifest
            .runtime
            .as_mut()
            .unwrap()
            .artifacts
            .get_mut(target().unwrap())
            .unwrap()
            .url = url;
        let bundle = Bundle::new(bundle.manifest, bundle.files).unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().into());
        store.download(bundle, client).await.unwrap();
        server.await.unwrap();
        assert!(store.selected().unwrap().is_some());
    }
    #[tokio::test]
    #[ignore = "requires isolated native rpi, HTTP cdylib and RPC fixture paths"]
    async fn downloaded_native_provider_starts_via_the_actual_rpi_abi() {
        let dll = std::env::var_os("RPI_TEST_NATIVE_DLL").expect("RPI_TEST_NATIVE_DLL");
        let rpi = std::env::var_os("RPI_TEST_RPI_BIN").expect("RPI_TEST_RPI_BIN");
        let rpc = std::env::var_os("RPI_TEST_RPC_BIN").expect("RPI_TEST_RPC_BIN");
        let bytes = std::fs::read(dll).unwrap();
        let (url, client, server) = tls_fixture(bytes.clone()).await;
        let mut bundle = package(&bytes);
        bundle
            .manifest
            .runtime
            .as_mut()
            .unwrap()
            .artifacts
            .get_mut(target().unwrap())
            .unwrap()
            .url = url;
        let bundle = Bundle::new(bundle.manifest, bundle.files).unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("components"));
        store.download(bundle, client).await.unwrap();
        server.await.unwrap();
        let provider = store.selected().unwrap().unwrap();
        let launch = crate::Launch {
            executable: rpi.into(),
            library: provider.library,
            arguments: vec![
                "--http".into(),
                "--bind".into(),
                "127.0.0.1".into(),
                "--port".into(),
                "0".into(),
                "--node-data".into(),
                root.path().join("node").into_os_string(),
            ],
            directory: Some(root.path().into()),
            environment: BTreeMap::from([
                (
                    "RPI_CODING_AGENT_DIR".into(),
                    root.path().join("agent").into_os_string(),
                ),
                ("RPI_WORK_RPI_BIN".into(), rpc),
                ("RPI_WORK_HTTP_PUBLIC_URL".into(), "".into()),
            ]),
        };
        // An absent public URL is not the same as an explicitly invalid empty one.
        let mut launch = launch;
        launch
            .environment
            .remove(std::ffi::OsStr::new("RPI_WORK_HTTP_PUBLIC_URL"));
        let (mut process, control) = crate::Process::start(launch).unwrap();
        let response = reqwest::Client::new()
            .get(format!("{}api/v1/health", control.ready.control_address))
            .header("x-rpi-node-control", control.capability())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        process.stop().unwrap();
        assert!(store.selected().unwrap().is_some());
    }
    fn package(bytes: &[u8]) -> Bundle {
        Bundle::new(
            Manifest {
                schema_version: 1,
                id: "test-node".into(),
                version: "1.0.0".into(),
                title: "Test provider".into(),
                description: String::new(),
                panels: vec![Panel {
                    id: "settings".into(),
                    title: "Provider".into(),
                    slot: "settings".into(),
                    entry: "index.html".into(),
                    protocol: 1,
                }],
                actions: BTreeMap::new(),
                runtime: Some(NativeRuntime {
                    entry_flag: "http".into(),
                    artifacts: BTreeMap::from([(
                        target().unwrap().into(),
                        NativeArtifact {
                            url: "https://downloads.example.com/native.dll".into(),
                            sha256: format!("{:x}", Sha256::digest(bytes)),
                            bytes: bytes.len() as u64,
                        },
                    )]),
                }),
            },
            BTreeMap::from([("index.html".into(), "<h1>Provider</h1>".into())]),
        )
        .unwrap()
    }
    async fn response(bytes: Vec<u8>) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8];
            while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                if socket.read(&mut byte).unwrap_or(0) == 0 {
                    return;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(
                !request.contains("authorization:")
                    && !request.contains("cookie:")
                    && !request.contains("x-rpi-node-control:")
            );
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = socket.write_all(&bytes);
        });
        reqwest::Client::new()
            .get(format!("http://{address}/runtime"))
            .send()
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn interrupted_or_corrupt_download_never_changes_the_selected_provider() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().into());
        let good = b"native-provider-test-bytes";
        let package = package(good);
        assert!(store.install(package.clone(), false).await.is_err());
        assert!(!root.path().join("selection.json").exists());
        store
            .stage_response(package.clone(), response(good.to_vec()).await)
            .await
            .unwrap();
        let selected = std::fs::read(root.path().join("selection.json")).unwrap();
        assert!(store.selected().unwrap().is_some());
        assert!(store
            .stage_response(package.clone(), response(vec![b'x'; good.len()]).await)
            .await
            .is_err());
        assert!(store
            .stage_response(package.clone(), response(vec![b'x'; good.len() + 1]).await)
            .await
            .is_err());
        assert!(store
            .stage_response(package.clone(), response(vec![b'x'; good.len() - 1]).await)
            .await
            .is_err());
        assert_eq!(
            std::fs::read(root.path().join("selection.json")).unwrap(),
            selected
        );
        // An update is immutable and cannot overwrite a currently loaded DLL.
        let old = store.selected().unwrap().unwrap().library;
        let mut update = package.clone();
        update.manifest.version = "1.0.1".into();
        let update = Bundle::new(update.manifest, update.files).unwrap();
        store
            .stage_response(update, response(good.to_vec()).await)
            .await
            .unwrap();
        assert_ne!(store.selected().unwrap().unwrap().library, old);
        assert_eq!(std::fs::read(&old).unwrap(), good);
        std::fs::write(&old, b"tampered").unwrap();
        std::fs::write(root.path().join("selection.json"), selected).unwrap();
        assert!(store.selected().is_err());
    }
}
