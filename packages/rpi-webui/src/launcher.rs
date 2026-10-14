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
    pub fn from_env() -> Result<Self, String> {
        let url = std::env::var("RPI_WEBUI_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
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
        let manage = env_bool(
            "RPI_WEBUI_MANAGE",
            url.as_str().trim_end_matches('/') == "http://127.0.0.1:8080",
        );
        if manage && !local {
            return Err("Managed Open WebUI must use a loopback URL".into());
        }
        let args: Vec<String> = match std::env::var("RPI_WEBUI_ARGS") {
            Ok(args) => serde_json::from_str(&args)
                .map_err(|e| format!("RPI_WEBUI_ARGS requires a JSON array of strings: {e}"))?,
            Err(_) => vec![
                "serve".into(),
                "--host".into(),
                "127.0.0.1".into(),
                "--port".into(),
                "8080".into(),
            ],
        };
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .ok_or("Cannot locate the user's home directory")?;
        Ok(Self {
            url,
            args,
            manage,
            command: std::env::var("RPI_WEBUI_COMMAND").unwrap_or_else(|_| "open-webui".into()),
            open_browser: env_bool("RPI_WEBUI_OPEN_BROWSER", true),
            log: home.join(".rpi/webui/openwebui.log"),
        })
    }
}

/// Only stop a process this invocation created; pre-existing servers keep running.
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.log)
            .map_err(|e| e.to_string())?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .env("RPI_WEBUI_AUTOSTART", "0")
            .env("RPI_WEBUI_CHILD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(log));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        owned.0 = Some(command.spawn().map_err(|e| {
            format!("Cannot start Open WebUI: {e}. Install Open WebUI or set RPI_WEBUI_COMMAND.")
        })?);
        let deadline = Instant::now() + Duration::from_secs(60);
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
    if config.open_browser {
        open::that(config.url.as_str()).map_err(|e| format!("Cannot open browser: {e}"))?;
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
mod tests {
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

    fn config(port: u16) -> Config {
        let dir =
            std::env::temp_dir().join(format!("rpi-webui-test-{}-{}", std::process::id(), port));
        Config {
            url: url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            command: "unused-command".into(),
            args: vec![],
            manage: false,
            open_browser: false,
            log: dir.join("openwebui.log"),
        }
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
