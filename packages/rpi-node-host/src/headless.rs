//! Service supervision without a GUI or remotely accessible shutdown endpoint.
use std::io::Read;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyStatus {
    ready: bool,
    address: String,
    node_id: String,
    sharing_enabled: bool,
    web_hosted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_address: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    warning: String,
}

impl ReadyStatus {
    fn from_sharing(info: &crate::http::Sharing) -> Result<Self, String> {
        if !info.listening {
            return Err(format!("HTTP startup failed: {}", info.status));
        }
        if info.address.is_empty() || info.node_id.is_empty() {
            return Err("HTTP startup did not publish its bound address and node identity".into());
        }
        Ok(Self {
            ready: true,
            address: info.address.clone(),
            node_id: info.node_id.clone(),
            sharing_enabled: info.enabled,
            web_hosted: info.web_hosted,
            control_address: None,
            // Do not serialize arbitrary diagnostic text or the Sharing object.
            // GUI callers retain the original warning in HTTP info.
            warning: if info.warning.is_empty() {
                String::new()
            } else {
                "Saved sharing preference could not be loaded; sharing remains disabled.".into()
            },
        })
    }
}

fn require_explicit_password(primary: Option<&str>, fallback: Option<&str>) -> Result<(), String> {
    match primary.or(fallback) {
        Some(value) if !value.is_empty() => Ok(()),
        _ => Err("Headless mode requires a nonempty explicit RPI_WORK_HTTP_PASSWORD or RPI_HTTP_PASSWORD".into()),
    }
}

/// Run the same local owner used by the GUI until Ctrl+C (also SIGTERM on Unix).
///
/// Requires an explicitly configured nonempty HTTP password BEFORE resolving or
/// locking user data; never logs it. Publishes one credential-free JSON
/// frame to stdout only after successful HTTP startup. Consumers must wait for
/// the complete LF: cancellation/timeout can leave an incomplete prefix.
/// Retains saved sharing
/// policy, including closed/fail-closed preferences: ready does not imply API
/// access (closed sharing returns HTTP 403).
///
/// `shutdown_stdin` is an explicit service-pipe supervision capability, intended
/// for a caller's `--shutdown-stdin` flag. Only when true does EOF stop the host;
/// inherited/closed stdin has no effect by default. There is no HTTP shutdown API.
/// Callers must map Err to a nonzero exit code, including unconfirmed shutdown.
pub async fn run_headless(shutdown_stdin: bool) -> Result<(), String> {
    run_headless_with(crate::LaunchOverrides::default(), shutdown_stdin).await
}

pub async fn run_headless_with(
    options: crate::LaunchOverrides,
    shutdown_stdin: bool,
) -> Result<(), String> {
    let primary = std::env::var("RPI_WORK_HTTP_PASSWORD").ok();
    let fallback = std::env::var("RPI_HTTP_PASSWORD").ok();
    require_explicit_password(
        options.http.password.as_deref().or(primary.as_deref()),
        fallback.as_deref(),
    )?;
    // Do not retain additional copies of credentials throughout service life.
    drop((primary, fallback));

    // Register before acquiring the data lock, not after publishing readiness:
    // a signal during synchronous startup must still take the cleanup path.
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    tokio::select! {
        biased;
        result = &mut interrupt => return result.map_err(|_| "Unable to listen for Ctrl+C".into()),
        () = std::future::ready(()) => {}
    }
    #[cfg(unix)]
    let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "Unable to listen for SIGTERM")?;

    let guard = crate::start_with(options).ok_or_else(|| {
        crate::startup_error()
            .unwrap_or("Local host startup failed")
            .to_owned()
    })?;
    let result = serve_until_shutdown(
        shutdown_stdin,
        &mut interrupt,
        #[cfg(unix)]
        terminate,
    )
    .await;
    let stopped = guard.checked_shutdown();
    match (result, stopped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown)) => Err(format!("{error}; {shutdown}")),
    }
}

/// Supervise a native rpi extension using its `SessionShutdown` signal. The plugin
/// owns stdio; ready output and cleanup retain the ordinary owner guarantees.
pub async fn run_extension(
    options: crate::LaunchOverrides,
    stop: impl std::future::Future<Output = ()>,
    shutdown_stdin: bool,
) -> Result<(), String> {
    let primary = std::env::var("RPI_WORK_HTTP_PASSWORD").ok();
    let fallback = std::env::var("RPI_HTTP_PASSWORD").ok();
    if options.control_capability.is_none() {
        require_explicit_password(
            options.http.password.as_deref().or(primary.as_deref()),
            fallback.as_deref(),
        )?;
    }
    drop((primary, fallback));
    let guard = crate::start_with(options).ok_or_else(|| {
        crate::startup_error()
            .unwrap_or("Node startup failed")
            .to_owned()
    })?;
    let result = async {
        let info = crate::http::sharing();
        let mut ready = if !info.listening && guard.control_address().is_some() {
            ReadyStatus {
                ready: true,
                address: String::new(),
                node_id: guard.node_id().into(),
                sharing_enabled: false,
                web_hosted: false,
                control_address: None,
                warning: "Public HTTP is unavailable; private local control remains available."
                    .into(),
            }
        } else {
            ReadyStatus::from_sharing(&info)?
        };
        ready.control_address = guard.control_address();
        let mut line = serde_json::to_vec(&ready).map_err(|_| "Unable to encode ready status")?;
        line.push(b'\n');
        let stdin = stdin_shutdown(shutdown_stdin)?;
        let stop = async {
            tokio::select! { () = stop => Ok(()), result = wait_stdin(stdin) => result }
        };
        tokio::pin!(stop);
        crate::ready_output::publish_and_wait(&line, stop.as_mut()).await
    }
    .await;
    let stopped = guard.checked_shutdown();
    match (result, stopped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
        (Err(e), Err(cleanup)) => Err(format!("{e}; {cleanup}")),
    }
}

async fn serve_until_shutdown(
    shutdown_stdin: bool,
    interrupt: impl std::future::Future<Output = std::io::Result<()>>,
    #[cfg(unix)] mut terminate: tokio::signal::unix::Signal,
) -> Result<(), String> {
    tokio::pin!(interrupt);
    let ready = ReadyStatus::from_sharing(&crate::http::sharing())?;
    let stdin = stdin_shutdown(shutdown_stdin)?;
    // No synchronous stderr diagnostics while the host owns live resources:
    // a full stderr pipe must not prevent EOF/signal supervision either.
    // Serialize an explicit allowlist, never Sharing (which contains a password).
    let mut line = serde_json::to_vec(&ready).map_err(|_| "Unable to encode ready status")?;
    line.push(b'\n');
    let stop = async {
        tokio::select! {
            biased;
            result = &mut interrupt => result.map_err(|_| "Unable to listen for Ctrl+C".into()),
            result = wait_stdin(stdin) => result,
            result = wait_terminate(
                #[cfg(unix)]
                &mut terminate,
            ) => result,
        }
    };
    tokio::pin!(stop);
    crate::ready_output::publish_and_wait(&line, stop.as_mut()).await
}

type StdinShutdown = tokio::sync::oneshot::Receiver<Result<(), String>>;

fn stdin_shutdown(enabled: bool) -> Result<Option<StdinShutdown>, String> {
    if !enabled {
        return Ok(None);
    }
    let (done, completed) = tokio::sync::oneshot::channel();
    // Tokio's stdin uses an uncancellable blocking-pool read, which can hang
    // caller-runtime destruction after a signal. A detached OS thread instead
    // cannot hold up runtime shutdown; it owns no workspace/HTTP/data resources.
    std::thread::Builder::new()
        .name("work-host-shutdown-stdin".into())
        .spawn(move || {
            let result = read_until_eof(std::io::stdin().lock());
            let _ = done.send(result);
        })
        .map_err(|_| "Unable to start stdin shutdown monitor")?;
    Ok(Some(completed))
}

fn read_until_eof(mut input: impl Read) -> Result<(), String> {
    let mut buffer = [0; 4096];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("Unable to read shutdown stdin".into()),
        }
    }
}

async fn wait_stdin(input: Option<StdinShutdown>) -> Result<(), String> {
    match input {
        Some(input) => input.await.map_err(|_| "Stdin shutdown monitor exited")?,
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
async fn wait_terminate(signal: &mut tokio::signal::unix::Signal) -> Result<(), String> {
    signal
        .recv()
        .await
        .ok_or_else(|| "SIGTERM monitor exited".into())
}

#[cfg(not(unix))]
async fn wait_terminate() -> Result<(), String> {
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_password_preflight_preserves_http_precedence() {
        assert!(require_explicit_password(None, None).is_err());
        assert!(require_explicit_password(Some(""), Some("fallback")).is_err());
        assert!(require_explicit_password(None, Some("")).is_err());
        assert!(require_explicit_password(Some("primary"), None).is_ok());
        assert!(require_explicit_password(None, Some("fallback")).is_ok());
    }

    #[test]
    fn ready_allowlist_never_serializes_credentials_or_qr_suggestions() {
        let info = crate::http::Sharing {
            listening: true,
            enabled: false,
            address: "http://127.0.0.1:45678/".into(),
            node_id: "stable-id".into(),
            password: "NEVER-PRINT".into(),
            public_url: "https://other.example/".into(),
            ..Default::default()
        };
        let line = serde_json::to_string(&ReadyStatus::from_sharing(&info).unwrap()).unwrap();
        assert!(!line.contains("NEVER-PRINT"));
        assert!(!line.contains("other.example"));
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 5);
        assert_eq!(json["ready"], true);
        assert_eq!(json["address"], "http://127.0.0.1:45678/");
        assert_eq!(json["nodeId"], "stable-id");
        assert_eq!(json["sharingEnabled"], false);
        assert_eq!(json["webHosted"], false);
    }

    #[test]
    fn ready_warning_is_optional_fixed_and_non_secret() {
        let info = crate::http::Sharing {
            listening: true,
            address: "http://127.0.0.1:45678/".into(),
            node_id: "stable-id".into(),
            password: "NEVER-PRINT".into(),
            warning: "untrusted diagnostic NEVER-PRINT".into(),
            ..Default::default()
        };
        let line = serde_json::to_string(&ReadyStatus::from_sharing(&info).unwrap()).unwrap();
        assert!(!line.contains("NEVER-PRINT"));
        assert!(!line.contains("untrusted"));
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut fields: Vec<_> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "address",
                "nodeId",
                "ready",
                "sharingEnabled",
                "warning",
                "webHosted"
            ]
        );
        assert_eq!(
            json["warning"],
            "Saved sharing preference could not be loaded; sharing remains disabled."
        );
        assert_eq!(json["sharingEnabled"], false);
    }

    #[test]
    fn failed_http_never_becomes_ready() {
        assert!(ReadyStatus::from_sharing(&crate::http::Sharing::default()).is_err());
    }

    #[tokio::test]
    async fn stdin_is_ignored_unless_explicitly_enabled() {
        assert!(stdin_shutdown(false).unwrap().is_none());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), wait_stdin(None))
                .await
                .is_err()
        );
        let (send, receive) = tokio::sync::oneshot::channel();
        send.send(Ok(())).unwrap();
        wait_stdin(Some(receive)).await.unwrap();
    }

    #[test]
    fn stdin_content_is_ignored_until_eof() {
        struct Failed;
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("sensitive details"))
            }
        }
        read_until_eof(std::io::Cursor::new(b"not a shutdown command\n")).unwrap();
        read_until_eof(std::io::empty()).unwrap();
        let error = read_until_eof(Failed).unwrap_err();
        assert!(!error.contains("sensitive details"));
    }
}
