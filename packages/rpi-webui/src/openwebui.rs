//! Open WebUI's administrative adapter. Never forwards credentials on redirects.
use crate::launcher::Config;
use reqwest::blocking::{Client, Response};
use serde_json::{json, Value};

fn value(response: Response, operation: &str) -> Result<Value, String> {
    if !response.status().is_success() {
        // Response bodies can contain configuration or credentials; keep errors redacted.
        return Err(format!(
            "Open WebUI {operation} failed (HTTP {})",
            response.status()
        ));
    }
    response
        .json()
        .map_err(|_| format!("Open WebUI {operation} returned invalid JSON"))
}

pub fn connect(config: &Config, owned: bool) -> Result<(), String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let endpoint = |path: &str| format!("{}{path}", config.url.as_str().trim_end_matches('/'));
    // Existing authenticated installations are only modified with an explicit admin token.
    let token = match config.admin_token.as_ref() {
        Some(token) => token.clone(),
        None if owned => {
            let info = value(
                client
                    .get(endpoint("/api/config"))
                    .send()
                    .map_err(|e| e.to_string())?,
                "configuration",
            )?;
            if info["features"]["auth"] != false {
                return Err(
                    "This Open WebUI requires an administrator token; set RPI_WEBUI_ADMIN_TOKEN"
                        .into(),
                );
            }
            let auth = value(
                client
                    .post(endpoint("/api/v1/auths/signin"))
                    .json(&json!({"email":"admin@localhost", "password":"admin"}))
                    .send()
                    .map_err(|e| e.to_string())?,
                "local signin",
            )?;
            auth["token"]
                .as_str()
                .ok_or("Open WebUI did not return a signin token")?
                .to_owned()
        }
        None => {
            eprintln!("[rpi-webui] Opening existing server. Set RPI_WEBUI_ADMIN_TOKEN once to configure its RPI adapter automatically.");
            return Ok(());
        }
    };
    let request = |method, path: &str| client.request(method, endpoint(path)).bearer_auth(&token);
    let id = &config.function_id;
    let path = format!("/api/v1/functions/id/{id}");
    // Open WebUI returns 401 for both missing functions and expired credentials.
    // List with admin authentication first, instead of interpreting 401 as absence.
    let functions = value(
        request(reqwest::Method::GET, "/api/v1/functions/list")
            .send()
            .map_err(|e| e.to_string())?,
        "list functions",
    )?;
    let exists = functions
        .as_array()
        .ok_or("Open WebUI function list is invalid")?
        .iter()
        .any(|function| function["id"] == id.as_str());
    let old = if exists {
        Some(value(
            request(reqwest::Method::GET, &path)
                .send()
                .map_err(|e| e.to_string())?,
            "get function",
        )?)
    } else {
        None
    };
    if !config.local
        && old.is_none()
        && (std::env::var("RPI_WEBUI_ACP_COMMAND").is_err()
            || std::env::var("RPI_WEBUI_REMOTE_WORKSPACE").is_err())
    {
        return Err("A new remote adapter requires RPI_WEBUI_ACP_COMMAND and RPI_WEBUI_REMOTE_WORKSPACE on that server".into());
    }
    // Each local workspace gets its own model so concurrent projects cannot redirect each other.
    let content = include_str!("../openwebui/rpi_acp_pipe.py");
    let payload = json!({"id":id,"name":"RPI ACP","content":content,
        "meta":{"description":"RPI agent through ACP, configured by rpi-webui"}});
    if old.as_ref().is_none_or(|old| old["content"] != content) {
        let update = if old.is_some() {
            format!("{path}/update")
        } else {
            "/api/v1/functions/create".into()
        };
        value(
            request(reqwest::Method::POST, &update)
                .json(&payload)
                .send()
                .map_err(|e| e.to_string())?,
            "install function",
        )?;
    }
    let mut valves = value(
        request(reqwest::Method::GET, &format!("{path}/valves"))
            .send()
            .map_err(|e| e.to_string())?,
        "get valves",
    )?;
    if !valves.is_object() {
        valves = json!({});
    }
    if config.local {
        valves["MODEL_NAME"] = json!(format!(
            "RPI · {}",
            config
                .workspace
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));
        valves["ACP_COMMAND"] = json!(std::env::current_exe().map_err(|e| e.to_string())?);
        valves["ACP_ARGS"] = json!(serde_json::to_string(&vec!["--webui-acp"]).unwrap());
        valves["WORKSPACE"] = json!(config.workspace);
        valves["STATE_DIR"] = json!(config.log.parent().unwrap().join("sessions"));
    }
    // Remote paths live on that server: preserve existing values, accept explicit overrides.
    for (key, env) in [
        ("ACP_COMMAND", "RPI_WEBUI_ACP_COMMAND"),
        ("ACP_ARGS", "RPI_WEBUI_ACP_ARGS"),
        ("WORKSPACE", "RPI_WEBUI_REMOTE_WORKSPACE"),
        ("STATE_DIR", "RPI_WEBUI_REMOTE_STATE_DIR"),
    ] {
        if let Ok(v) = std::env::var(env) {
            valves[key] = json!(v);
        }
    }
    if config.local || old.is_none() || config.permission_explicit {
        valves["PERMISSION_MODE"] = json!(config.permission);
    }
    value(
        request(reqwest::Method::POST, &format!("{path}/valves/update"))
            .json(&valves)
            .send()
            .map_err(|e| e.to_string())?,
        "configure function",
    )?;
    // Re-read activation state; source updates preserve it, new functions start inactive.
    let state = value(
        request(reqwest::Method::GET, &path)
            .send()
            .map_err(|e| e.to_string())?,
        "get activation",
    )?;
    if state["is_active"] != true {
        value(
            request(reqwest::Method::POST, &format!("{path}/toggle"))
                .send()
                .map_err(|e| e.to_string())?,
            "activate function",
        )?;
    }
    eprintln!(
        "[rpi-webui] Connected RPI model {id}.rpi ({})",
        config.permission
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn fixture(
        steps: Vec<(&'static str, &'static str, u16, Value)>,
    ) -> (Config, std::thread::JoinHandle<Vec<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut cfg = crate::launcher::tests::config(listener.local_addr().unwrap().port());
        cfg.function_id = "rpi_test".into();
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for (method, path, status, response) in steps {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let headers = loop {
                    let mut buf = [0; 4096];
                    let count = stream.read(&mut buf).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buf[..count]);
                    if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let head = String::from_utf8_lossy(&bytes[..headers]);
                assert!(
                    head.starts_with(&format!("{method} {path} HTTP/1.1")),
                    "{head}"
                );
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < headers + length {
                    let mut buf = [0; 4096];
                    let count = stream.read(&mut buf).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buf[..count]);
                }
                bodies.push(if length > 0 {
                    serde_json::from_slice(&bytes[headers..headers + length]).unwrap()
                } else {
                    Value::Null
                });
                let json = response.to_string();
                write!(stream, "HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).unwrap();
            }
            bodies
        });
        (cfg, server)
    }

    #[test]
    fn first_launch_installs_configures_and_activates_without_a_separate_bridge() {
        let (mut cfg, server) = fixture(vec![
            (
                "GET",
                "/api/config",
                200,
                json!({"features":{"auth":false}}),
            ),
            (
                "POST",
                "/api/v1/auths/signin",
                200,
                json!({"token":"fixture"}),
            ),
            ("GET", "/api/v1/functions/list", 200, json!([])),
            (
                "POST",
                "/api/v1/functions/create",
                200,
                json!({"is_active":false}),
            ),
            (
                "GET",
                "/api/v1/functions/id/rpi_test/valves",
                200,
                Value::Null,
            ),
            (
                "POST",
                "/api/v1/functions/id/rpi_test/valves/update",
                200,
                json!({}),
            ),
            (
                "GET",
                "/api/v1/functions/id/rpi_test",
                200,
                json!({"is_active":false}),
            ),
            (
                "POST",
                "/api/v1/functions/id/rpi_test/toggle",
                200,
                json!({"is_active":true}),
            ),
        ]);
        cfg.permission = "bypass".into();
        connect(&cfg, true).unwrap();
        let calls = server.join().unwrap();
        assert_eq!(
            calls[3]["content"],
            include_str!("../openwebui/rpi_acp_pipe.py")
        );
        assert_eq!(calls[5]["ACP_ARGS"], r#"["--webui-acp"]"#);
        assert_eq!(
            calls[5]["ACP_COMMAND"],
            json!(std::env::current_exe().unwrap())
        );
        assert_eq!(calls[5]["WORKSPACE"], json!(cfg.workspace));
        assert_eq!(calls[5]["PERMISSION_MODE"], "bypass");
    }

    #[test]
    fn repeat_connection_keeps_remote_paths_permissions_and_other_valves() {
        let (mut cfg, server) = fixture(vec![
            (
                "GET",
                "/api/v1/functions/list",
                200,
                json!([{"id":"rpi_test"}]),
            ),
            (
                "GET",
                "/api/v1/functions/id/rpi_test",
                200,
                json!({"is_active":true,"content":include_str!("../openwebui/rpi_acp_pipe.py")}),
            ),
            (
                "GET",
                "/api/v1/functions/id/rpi_test/valves",
                200,
                json!({"ACP_COMMAND":"/server/bridge","WORKSPACE":"/server/project","PERMISSION_MODE":"bypass","TURN_TIMEOUT":999}),
            ),
            (
                "POST",
                "/api/v1/functions/id/rpi_test/valves/update",
                200,
                json!({}),
            ),
            (
                "GET",
                "/api/v1/functions/id/rpi_test",
                200,
                json!({"is_active":true}),
            ),
        ]);
        cfg.local = false;
        cfg.admin_token = Some("fixture".into());
        connect(&cfg, false).unwrap();
        let calls = server.join().unwrap();
        assert_eq!(calls[3]["ACP_COMMAND"], "/server/bridge");
        assert_eq!(calls[3]["WORKSPACE"], "/server/project");
        assert_eq!(calls[3]["PERMISSION_MODE"], "bypass");
        assert_eq!(calls[3]["TURN_TIMEOUT"], 999);
    }

    #[test]
    fn invalid_admin_token_does_not_attempt_to_create_a_function() {
        let (mut cfg, server) = fixture(vec![(
            "GET",
            "/api/v1/functions/list",
            401,
            json!({"detail":"redacted-secret"}),
        )]);
        cfg.admin_token = Some("fixture".into());
        let error = connect(&cfg, false).unwrap_err();
        assert!(error.contains("401"));
        assert!(!error.contains("redacted-secret"));
        assert_eq!(server.join().unwrap().len(), 1);
    }
}
