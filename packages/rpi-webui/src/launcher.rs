use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub struct Config {
    pub url: url::Url,
    pub command: String,
    pub args: Vec<String>,
    pub manage: bool,
    pub open_browser: bool,
    pub log: PathBuf,
    pub local: bool,
    pub workspace: PathBuf,
    pub permission: String,
    pub permission_explicit: bool,
    pub admin_token: Option<String>,
    pub function_id: String,
    pub configure: bool,
}

pub fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name).ok().map_or(default, |value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

impl Config {
    pub fn browser_url(&self) -> url::Url {
        let mut url = self.url.clone();
        url.query_pairs_mut()
            .append_pair("model", &format!("{}.rpi", self.function_id));
        url
    }

    pub fn from_options(flag: impl Fn(&str) -> Option<serde_json::Value>) -> Result<Self, String> {
        let option = |name: &str, env: &str| -> Result<Option<String>, String> {
            if let Some(value) = flag(name).filter(|v| !v.is_null()) {
                return value
                    .as_str()
                    .map(|s| Some(s.to_owned()))
                    .ok_or_else(|| format!("--{name} requires a value"));
            }
            Ok(std::env::var(env).ok())
        };
        let adapter = match flag("webui") {
            Some(serde_json::Value::String(name))
                if !matches!(name.as_str(), "" | "true" | "1") =>
            {
                name
            }
            _ => std::env::var("RPI_WEBUI_UI").unwrap_or_else(|_| "openwebui".into()),
        };
        if !matches!(adapter.as_str(), "openwebui" | "open-webui") {
            return Err(format!(
                "Unsupported WebUI adapter '{adapter}'; currently supported: openwebui"
            ));
        }
        let url =
            option("webui-url", "RPI_WEBUI_URL")?.unwrap_or_else(|| "http://127.0.0.1:8080".into());
        let url = url::Url::parse(&url).map_err(|e| e.to_string())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("RPI_WEBUI_URL must be an HTTP(S) URL without embedded credentials, query or fragment".into());
        }
        let local = url.host().is_some_and(|host| match host {
            url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        });
        let manage = env_bool("RPI_WEBUI_MANAGE", local);
        if manage && !local {
            return Err("Managed Open WebUI must use a loopback URL".into());
        }
        let args: Vec<String> = match std::env::var("RPI_WEBUI_ARGS") {
            Ok(args) => serde_json::from_str(&args)
                .map_err(|e| format!("RPI_WEBUI_ARGS requires a JSON array of strings: {e}"))?,
            Err(_) => vec![
                "serve".into(),
                "--host".into(),
                url.host_str().unwrap().trim_matches(['[', ']']).into(),
                "--port".into(),
                url.port_or_known_default().unwrap().to_string(),
            ],
        };
        if manage
            && !args.windows(2).any(|a| {
                a[0] == "--host" && matches!(a[1].as_str(), "127.0.0.1" | "::1" | "localhost")
            })
        {
            return Err("Managed single-user Open WebUI requires --host 127.0.0.1 (or another loopback host)".into());
        }
        if manage
            && args.windows(2).any(|a| {
                a[0] == "--host" && !matches!(a[1].as_str(), "127.0.0.1" | "::1" | "localhost")
            })
        {
            return Err(
                "Managed single-user Open WebUI cannot listen on a public interface".into(),
            );
        }
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .ok_or("Cannot locate the user's home directory")?;
        let workspace = option("webui-workspace", "RPI_WEBUI_WORKSPACE")?
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
        let workspace = workspace
            .canonicalize()
            .map_err(|e| format!("Cannot open WebUI workspace: {e}"))?;
        if !workspace.is_dir() {
            return Err("WebUI workspace must be a directory".into());
        }
        // Windows verbatim paths do not compare normally in Python; retain a standard absolute path.
        let workspace = if cfg!(windows) {
            let text = workspace.to_string_lossy();
            if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
                PathBuf::from(format!(r"\\{unc}"))
            } else {
                PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
            }
        } else {
            workspace
        };
        let permission = option("webui-permission", "RPI_WEBUI_PERMISSION")?;
        let permission_explicit = permission.is_some();
        let permission = permission.unwrap_or_else(|| "ask".into());
        if !matches!(permission.as_str(), "ask" | "bypass") {
            return Err("--webui-permission requires ask or bypass".into());
        }
        let function_id = if local {
            workspace_id(&workspace)
        } else {
            "rpi_acp".into()
        };
        let admin_token = std::env::var("RPI_WEBUI_ADMIN_TOKEN")
            .ok()
            .filter(|s| !s.is_empty());
        Ok(Self {
            url,
            args,
            manage,
            command: std::env::var("RPI_WEBUI_COMMAND").unwrap_or_else(|_| "open-webui".into()),
            open_browser: env_bool("RPI_WEBUI_OPEN_BROWSER", true),
            log: home.join(".rpi/webui/openwebui.log"),
            local,
            workspace,
            permission,
            permission_explicit,
            configure: manage || admin_token.is_some(),
            admin_token,
            function_id,
        })
    }
}

fn workspace_id(path: &std::path::Path) -> String {
    // Stable FNV-1a, independent of Rust's randomized/unspecified DefaultHasher.
    let hash = path
        .to_string_lossy()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
    format!("rpi_acp_{hash:016x}")
}

/// Only stop a process this invocation created; pre-existing servers keep running.
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            #[cfg(windows)]
            if child.try_wait().is_ok_and(|status| status.is_none()) {
                // Python console scripts and uv may own a child Python process.
                use std::os::windows::process::CommandExt;
                let _ = Command::new("taskkill")
                    .args(["/PID", &child.id().to_string(), "/T", "/F"])
                    .creation_flags(0x08000000)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn server_command(config: &Config, uv: bool) -> Result<Command, String> {
    let mut command = Command::new(if uv { "uv" } else { &config.command });
    if uv {
        command
            .args([
                "tool",
                "run",
                "--python",
                "3.12",
                "--from",
                "open-webui==0.12.0",
                "open-webui",
            ])
            .env(
                "UV_INDEX_URL",
                std::env::var("UV_INDEX_URL")
                    .unwrap_or_else(|_| "https://pypi.tuna.tsinghua.edu.cn/simple".into()),
            );
    }
    command
        .args(&config.args)
        .current_dir(config.log.parent().unwrap())
        .env("RPI_WEBUI_CHILD", "1")
        .env("RPI_WEBUI_AUTOSTART", "0")
        .env("DATA_DIR", config.log.parent().unwrap().join("data"))
        .env(
            "CORS_ALLOW_ORIGIN",
            config.url.origin().ascii_serialization(),
        )
        .env_remove("DATABASE_URL")
        .env("WEBUI_AUTH", "false")
        .env("ENABLE_SIGNUP", "false")
        .env("OFFLINE_MODE", "true")
        .env("ENABLE_OPENAI_API", "false")
        .env("ENABLE_OLLAMA_API", "false")
        .env("DEFAULT_MODELS", format!("{}.rpi", config.function_id))
        .env("ENABLE_EVALUATION_ARENA_MODELS", "false")
        .env("ENABLE_TITLE_GENERATION", "false")
        .env("ENABLE_TAGS_GENERATION", "false")
        .env("ENABLE_FOLLOW_UP_GENERATION", "false")
        .env("ENABLE_AUTOCOMPLETE_GENERATION", "false")
        .stdin(Stdio::null());
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.log)
        .map_err(|e| e.to_string())?;
    command
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(command)
}

pub fn run(config: Config, stop: Receiver<()>) -> Result<(), String> {
    // Keep the Pipe in the crate so cargo and prebuilt installations have the same source.
    if let Some(directory) = config.log.parent() {
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let pipe = directory.join(concat!("rpi_acp_pipe-", env!("CARGO_PKG_VERSION"), ".py"));
        std::fs::write(&pipe, include_str!("../openwebui/rpi_acp_pipe.py"))
            .map_err(|e| e.to_string())?;
        eprintln!(
            "[rpi-webui] ACP Pipe for Open WebUI Functions: {}",
            pipe.display()
        );
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| e.to_string())?;
    let ready = || {
        client
            .get(config.url.clone())
            .send()
            .is_ok_and(|response| response.status().is_success())
    };
    let mut owned = OwnedChild(None);
    if !ready() {
        if !config.manage {
            return Err(format!(
                "Open WebUI is unavailable at {}; start it first or configure RPI_WEBUI_MANAGE",
                config.url
            ));
        }
        if let Some(parent) = config.log.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        owned.0 = Some(match server_command(&config, false)?.spawn() {
            Ok(child) => child,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound && config.command == "open-webui" =>
            {
                eprintln!("[rpi-webui] Installing Open WebUI with uv (Tsinghua mirror); first launch may take several minutes.");
                server_command(&config, true)?.spawn().map_err(|e| {
                    format!("Cannot start Open WebUI: {e}. Install uv or put open-webui on PATH.")
                })?
            }
            Err(e) => return Err(format!("Cannot start Open WebUI: {e}")),
        });
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            if let Some(status) = owned
                .0
                .as_mut()
                .unwrap()
                .try_wait()
                .map_err(|e| e.to_string())?
            {
                return Err(format!(
                    "Open WebUI exited with {status}; see {}",
                    config.log.display()
                ));
            }
            if ready() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Open WebUI startup timed out; see {}",
                    config.log.display()
                ));
            }
            match stop.recv_timeout(Duration::from_millis(250)) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => return Ok(()),
            }
        }
    }
    if config.configure {
        crate::openwebui::connect(&config, owned.0.is_some() || config.manage)?;
    }
    if config.open_browser {
        open::that(config.browser_url().as_str())
            .map_err(|e| format!("Cannot open browser: {e}"))?;
    }
    loop {
        match stop.recv_timeout(Duration::from_secs(1)) {
            Err(RecvTimeoutError::Timeout) => {
                if let Some(child) = &mut owned.0 {
                    if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                        return Err(format!(
                            "Open WebUI exited with {status}; see {}",
                            config.log.display()
                        ));
                    }
                }
            }
            _ => return Ok(()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn reply(mut stream: TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
    }

    pub(crate) fn config(port: u16) -> Config {
        let dir =
            std::env::temp_dir().join(format!("rpi-webui-test-{}-{}", std::process::id(), port));
        Config {
            url: url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            command: "unused-command".into(),
            args: vec![],
            manage: false,
            open_browser: false,
            log: dir.join("openwebui.log"),
            local: true,
            workspace: std::env::current_dir().unwrap(),
            permission: "ask".into(),
            permission_explicit: false,
            admin_token: None,
            function_id: "rpi_acp_test".into(),
            configure: false,
        }
    }

    #[test]
    fn cli_selects_adapter_workspace_and_permissions() {
        let cfg = Config::from_options(|name| match name {
            "webui" => Some(serde_json::json!("openwebui")),
            "webui-url" => Some(serde_json::json!("https://example.com")),
            "webui-permission" => Some(serde_json::json!("bypass")),
            "webui-workspace" => Some(serde_json::json!(std::env::current_dir().unwrap())),
            _ => None,
        })
        .unwrap();
        assert!(!cfg.manage);
        assert!(!cfg.local);
        assert!(cfg.permission_explicit);
        assert_eq!(cfg.permission, "bypass");
        assert!(cfg.browser_url().as_str().contains("model=rpi_acp.rpi"));
        assert!(Config::from_options(
            |name| (name == "webui").then(|| serde_json::json!("unknown-ui"))
        )
        .is_err());
        assert!(Config::from_options(
            |name| (name == "webui-permission").then(|| serde_json::json!(true))
        )
        .is_err());
        assert!(Config::from_options(
            |name| (name == "webui-permission").then(|| serde_json::json!("bad"))
        )
        .is_err());
    }

    #[test]
    fn reuses_existing_server_and_leaves_it_alive_on_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            // First is the launcher's readiness request; second proves it survived shutdown.
            for _ in 0..2 {
                reply(listener.accept().unwrap().0);
            }
        });
        let cfg = config(port);
        let exported = cfg.log.parent().unwrap().join(concat!(
            "rpi_acp_pipe-",
            env!("CARGO_PKG_VERSION"),
            ".py"
        ));
        let (stop, receiver) = std::sync::mpsc::channel();
        let launcher = std::thread::spawn(move || run(cfg, receiver));
        while !exported.exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.send(()).unwrap();
        launcher.join().unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(exported).unwrap(),
            include_str!("../openwebui/rpi_acp_pipe.py")
        );
        let result = reqwest::blocking::get(format!("http://127.0.0.1:{port}")).unwrap();
        assert!(result.status().is_success());
        server.join().unwrap();
    }

    #[test]
    fn missing_external_server_reports_error_without_spawning() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let (_stop, receiver) = std::sync::mpsc::channel();
        assert!(run(config(port), receiver)
            .unwrap_err()
            .contains("unavailable"));
    }

    #[test]
    fn starts_owned_server_and_stops_it_on_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // This variable is read only by the ignored fixture in the separate child process.
        std::env::set_var("RPI_WEBUI_TEST_PORT", port.to_string());
        let mut cfg = config(port);
        cfg.manage = true;
        cfg.command = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        cfg.args = vec![
            "--ignored".into(),
            "--exact".into(),
            "launcher::tests::fixture_http_server".into(),
        ];
        let (stop, receiver) = std::sync::mpsc::channel();
        let launcher = std::thread::spawn(move || run(cfg, receiver));
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "server did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        stop.send(()).unwrap();
        launcher.join().unwrap().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
        std::env::remove_var("RPI_WEBUI_TEST_PORT");
    }

    #[test]
    #[ignore = "HTTP child fixture launched by starts_owned_server_and_stops_it_on_shutdown"]
    fn fixture_http_server() {
        let port: u16 = std::env::var("RPI_WEBUI_TEST_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        for stream in listener.incoming() {
            reply(stream.unwrap());
        }
    }
}
