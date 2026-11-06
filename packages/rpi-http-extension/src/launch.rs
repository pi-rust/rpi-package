use rpi_node_host::LaunchOverrides;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const HELP: &str = "rpi --http[=CONFIG|help] [--bind IP] [--port PORT] [--node-data DIRECTORY] [--http-assets DIRECTORY]\nRequires RPI_HTTP_PASSWORD or RPI_WORK_HTTP_PASSWORD.\nThe native extension owns one workspace; clients connect to its published address.";

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    data_dir: Option<PathBuf>,
    rpi_bin: Option<PathBuf>,
    bind: Option<std::net::IpAddr>,
    port: Option<u16>,
    assets: Option<PathBuf>,
    password_env: Option<String>,
}

pub fn enabled(value: Option<&serde_json::Value>) -> bool {
    match value {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => false,
        Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("false") => false,
        _ => true,
    }
}
fn path(base: &Path, value: PathBuf) -> PathBuf {
    if value.is_absolute() {
        value
    } else {
        base.join(value)
    }
}

/// Parsed CLI values are data. No process environment is mutated by a plugin.
pub fn from_flags(flags: &BTreeMap<String, String>) -> Result<LaunchOverrides, String> {
    let cwd = std::env::current_dir().map_err(|_| "Cannot resolve current directory")?;
    let (config, base) = match flags
        .get("http")
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("true"))
    {
        Some(file) => {
            let file = path(&cwd, file.into());
            let meta = std::fs::metadata(&file).map_err(|_| "Cannot read HTTP configuration")?;
            if meta.len() > 128 * 1024 {
                return Err("HTTP configuration is too large".into());
            }
            let config: Config = serde_json::from_slice(
                &std::fs::read(&file).map_err(|_| "Cannot read HTTP configuration")?,
            )
            .map_err(|_| "Invalid HTTP configuration")?;
            (
                config,
                file.parent()
                    .ok_or("Invalid configuration path")?
                    .to_owned(),
            )
        }
        None => (Config::default(), cwd.clone()),
    };
    let password = if let Some(name) = config.password_env {
        if name.is_empty()
            || name.len() > 128
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err("Invalid passwordEnv name".into());
        }
        Some(
            std::env::var(name)
                .map_err(|_| "Configured password environment variable is missing")?,
        )
    } else {
        None
    };
    let rpi_bin = config
        .rpi_bin
        .map(|p| {
            if p.components().count() == 1 {
                p
            } else {
                path(&base, p)
            }
        })
        .or_else(|| std::env::var_os("RPI_WORK_RPI_BIN").map(PathBuf::from))
        .or_else(|| std::env::current_exe().ok());
    let data = flags
        .get("node-data")
        .map(|p| path(&cwd, p.into()))
        .or_else(|| config.data_dir.map(|p| path(&base, p)));
    let assets = flags
        .get("http-assets")
        .map(|p| path(&cwd, p.into()))
        .or_else(|| config.assets.map(|p| path(&base, p)));
    let bind = flags
        .get("bind")
        .map(|s| s.parse().map_err(|_| "Invalid bind IP"))
        .transpose()?
        .or(config.bind);
    let port = flags
        .get("port")
        .map(|s| s.parse().map_err(|_| "Invalid port"))
        .transpose()?
        .or(config.port);
    let control_capability = if flags
        .get("node-control-stdin")
        .is_some_and(|v| !v.is_empty() && !v.eq_ignore_ascii_case("false"))
    {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("rpi-control-handoff".into())
            .spawn(move || {
                let _ = send.send(read_control(&mut std::io::stdin().lock()));
            })
            .map_err(|_| "Cannot read private control handoff")?;
        Some(
            receive
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| "Private control handoff timeout")??,
        )
    } else {
        None
    };
    Ok(LaunchOverrides {
        rpi_bin,
        data,
        control_capability,
        http: rpi_node_host::http::LaunchOverrides {
            bind,
            port,
            assets,
            password,
        },
    })
}

fn read_control(input: &mut impl std::io::Read) -> Result<String, String> {
    use std::io::Read;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Handoff {
        capability: String,
    }
    let mut line = Vec::new();
    for byte in std::io::BufReader::new(input).take(256).bytes() {
        let byte = byte.map_err(|_| "Private control handoff failed")?;
        if byte == b'\n' {
            let handoff: Handoff =
                serde_json::from_slice(&line).map_err(|_| "Invalid private control handoff")?;
            if handoff.capability.len() != 64
                || !handoff.capability.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("Invalid private control capability".into());
            }
            return Ok(handoff.capability);
        }
        line.push(byte);
    }
    Err("Private control handoff missing or too large".into())
}

pub fn from_args(
    args: impl IntoIterator<Item = String>,
) -> Result<(LaunchOverrides, bool), String> {
    let mut flags = BTreeMap::new();
    let mut args = args.into_iter();
    let mut shutdown = false;
    while let Some(arg) = args.next() {
        if arg == "--shutdown-stdin" {
            shutdown = true;
            continue;
        }
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name.to_owned(), value.to_owned()),
            None => (arg, args.next().ok_or("Missing option value")?),
        };
        let name = match name.as_str() {
            "--config" => "http",
            "--bind" => "bind",
            "--port" => "port",
            "--node-data" => "node-data",
            "--http-assets" => "http-assets",
            _ => return Err("Unknown HTTP node option".into()),
        };
        if flags.insert(name.into(), value).is_some() {
            return Err("Duplicate HTTP node option".into());
        }
    }
    Ok((from_flags(&flags)?, shutdown))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_handoff_is_bounded_and_never_echoes_secrets() {
        let valid = format!("{{\"capability\":\"{}\"}}\n", "a".repeat(64));
        assert_eq!(read_control(&mut valid.as_bytes()).unwrap(), "a".repeat(64));
        assert!(read_control(&mut valid.trim_end().as_bytes()).is_err());
        assert!(read_control(&mut &vec![b'x'; 300][..]).is_err());
        assert!(
            !read_control(&mut &b"{\"capability\":\"private-secret\"}\n"[..])
                .unwrap_err()
                .contains("private-secret")
        );
    }
    #[test]
    fn explicit_options_are_validated_without_environment_writes() {
        let flags = BTreeMap::from([
            ("bind".into(), "127.0.0.1".into()),
            ("port".into(), "0".into()),
        ]);
        let options = from_flags(&flags).unwrap();
        assert_eq!(options.http.port, Some(0));
        assert!(options.http.bind.unwrap().is_loopback());
        assert!(from_flags(&BTreeMap::from([("port".into(), "70000".into())])).is_err());
        assert!(!enabled(Some(&serde_json::json!(false))));
        assert!(!enabled(None));
        assert!(enabled(Some(&serde_json::json!(true))));
    }
}
