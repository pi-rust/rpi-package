//! Standalone API configuration. Frontend settings remain in rpi-web.
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const HELP: &str = "rpi-httpcore [--config PATH] [--project PATH] [--rpi-bin PATH]
         [--bind IP] [--port N] [--registry PATH] [--session-dir PATH]
         [--session-timeout SECONDS] [--admission-limit N] [--add-project PATH]... [--projects-root PATH] [--help] [--version]

Serves /api/v1 over HTTP/SSE without frontend assets.
JSON paths are relative to the config file; CLI paths use the working directory.
CLI options override JSON. Bare executable names use PATH.
Authentication: RPI_HTTP_PASSWORD and RPI_HTTP_ALLOWED_HOSTS.
Legacy RPI_WEB_* variables remain accepted.";

fn default_service_name() -> &'static str {
    "rpi-http"
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AppConfig {
    #[serde(skip, default = "default_service_name")]
    pub service_name: &'static str,
    pub project: PathBuf,
    /// Additional workspace roots explicitly made available to clients.
    pub projects: Vec<PathBuf>,
    pub projects_root: Option<PathBuf>,
    pub rpi_bin: PathBuf,
    pub bind: String,
    pub port: u16,
    /// Optional Host allowlist; RPI_WEB_ALLOWED_HOSTS takes precedence.
    pub allowed_hosts: Option<Vec<String>>,
    /// Optional `--session-dir` forwarded to the native host.
    pub session_dir: Option<PathBuf>,
    /// Where the bridge remembers session ids across restarts.
    pub registry: PathBuf,
    /// Optional idle timeout for live native hosts, in seconds. `None` keeps
    /// the legacy behavior and retains live hosts until explicit close.
    pub session_timeout_seconds: Option<u64>,
    /// Maximum number of concurrent API owner operations.
    pub admission_limit: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            service_name: "rpi-http",
            project: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            projects: Vec::new(),
            projects_root: None,
            rpi_bin: PathBuf::from("rpi"),
            bind: "127.0.0.1".into(),
            port: 3030,
            allowed_hosts: None,
            session_dir: None,
            registry: rpi_extcore::registry::default_path(),
            session_timeout_seconds: None,
            admission_limit: 64,
        }
    }
}

#[derive(Debug)]
pub enum Launch {
    Run(Box<AppConfig>),
    Help,
    Version,
}

pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Launch, String> {
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot read working directory: {error}"))?;
    from_args_at(args, &cwd)
}

fn from_args_at(args: impl IntoIterator<Item = String>, cwd: &Path) -> Result<Launch, String> {
    let args = args.into_iter().collect::<Vec<_>>();
    let mut config_file = None;
    let mut options = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value.to_owned()))
            });
        match name {
            "--help" | "-h" => return Ok(Launch::Help),
            "--version" | "-V" => return Ok(Launch::Version),
            "--config" | "--project" | "--rpi-bin" | "--bind" | "--port" | "--registry"
            | "--session-dir" | "--session-timeout" | "--admission-limit" | "--add-project"
            | "--projects-root" => {
                let value = if let Some(value) = inline {
                    value
                } else {
                    index += 1;
                    args.get(index)
                        .filter(|value| !value.starts_with("--"))
                        .cloned()
                        .ok_or_else(|| format!("{name} requires a value"))?
                };
                if value.is_empty() {
                    return Err(format!("{name} requires a value"));
                }
                if name == "--config" {
                    config_file = Some(PathBuf::from(value));
                } else {
                    options.push((name.to_owned(), value));
                }
            }
            _ => return Err(format!("unknown option: {argument}")),
        }
        index += 1;
    }
    let mut config = if let Some(path) = config_file {
        let path = resolve(cwd, &path);
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let mut config: AppConfig = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        config.project = resolve(base, &config.project);
        config.registry = resolve(base, &config.registry);
        for project in &mut config.projects {
            *project = resolve(base, project);
        }
        config.rpi_bin = resolve_executable(base, &config.rpi_bin);
        for path in [&mut config.session_dir, &mut config.projects_root]
            .into_iter()
            .flatten()
        {
            *path = resolve(base, path);
        }
        config
    } else {
        AppConfig {
            project: cwd.to_path_buf(),
            ..AppConfig::default()
        }
    };
    for (name, value) in options {
        match name.as_str() {
            "--project" => config.project = resolve(cwd, Path::new(&value)),
            "--rpi-bin" => config.rpi_bin = resolve_executable(cwd, Path::new(&value)),
            "--bind" => config.bind = value,
            "--port" => {
                config.port = value
                    .parse()
                    .map_err(|_| "--port must be between 0 and 65535")?
            }
            "--registry" => config.registry = resolve(cwd, Path::new(&value)),
            "--session-dir" => config.session_dir = Some(resolve(cwd, Path::new(&value))),
            "--session-timeout" => {
                config.session_timeout_seconds = parse_session_timeout(&value)?;
            }
            "--admission-limit" => {
                config.admission_limit = parse_admission_limit(&value)?;
            }
            "--add-project" => config.projects.push(resolve(cwd, Path::new(&value))),
            "--projects-root" => config.projects_root = Some(resolve(cwd, Path::new(&value))),
            _ => unreachable!(),
        }
    }
    Ok(Launch::Run(Box::new(config)))
}

fn parse_session_timeout(value: &str) -> Result<Option<u64>, String> {
    let seconds = value
        .parse::<u64>()
        .map_err(|_| "--session-timeout must be a non-negative number of seconds".to_string())?;
    Ok((seconds > 0).then_some(seconds))
}

fn parse_admission_limit(value: &str) -> Result<u32, String> {
    let limit = value
        .parse::<u32>()
        .map_err(|_| "--admission-limit must be a positive number".to_string())?;
    if limit == 0 {
        return Err("--admission-limit must be a positive number".to_string());
    }
    Ok(limit)
}

fn resolve_executable(base: &Path, path: &Path) -> PathBuf {
    // Preserve a bare command for PATH lookup, including custom executable names.
    if path.components().count() > 1 || path.to_string_lossy().contains(['/', '\\']) {
        resolve(base, path)
    } else {
        path.to_path_buf()
    }
}

/// Expand a leading `~` or `~/` with the service user's home directory. JSON
/// and quoted CLI values are not shell-expanded, so a literal `~/Projects`
/// would otherwise resolve to a directory named `~` beside the config file.
fn expand_home(path: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    let rest = text.strip_prefix('~')?;
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return None;
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?;
    let rest = rest.trim_start_matches(['/', '\\']);
    Some(if rest.is_empty() {
        home
    } else {
        home.join(rest)
    })
}

fn resolve(base: &Path, path: &Path) -> PathBuf {
    let path = expand_home(path).unwrap_or_else(|| path.to_path_buf());
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_paths_and_cli_precedence_are_independent_of_argument_order() {
        let directory =
            std::env::temp_dir().join(format!("rpi-http-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("http.json");
        std::fs::write(&file,r#"{"project":"workspace","rpiBin":"bin/rpi","port":3030,"projects":["extra"],"sessionDir":"history"}"#).unwrap();
        let Launch::Run(config) = from_args([
            "--port".into(),
            "0".into(),
            "--config".into(),
            file.to_string_lossy().into(),
            "--add-project=other".into(),
        ])
        .unwrap() else {
            panic!("expected server config")
        };
        assert_eq!(config.port, 0);
        assert_eq!(config.service_name, "rpi-http");
        assert_eq!(config.project, directory.join("workspace"));
        assert_eq!(config.rpi_bin, directory.join("bin/rpi"));
        assert_eq!(
            config.projects,
            [
                directory.join("extra"),
                std::env::current_dir().unwrap().join("other")
            ]
        );
        assert_eq!(config.session_dir, Some(directory.join("history")));
        let Launch::Run(config) = from_args(["--session-timeout".into(), "120".into()]).unwrap()
        else {
            panic!("expected server config")
        };
        assert_eq!(config.session_timeout_seconds, Some(120));
        let Launch::Run(config) = from_args(["--session-timeout".into(), "0".into()]).unwrap()
        else {
            panic!("expected server config")
        };
        assert_eq!(config.session_timeout_seconds, None);
        let Launch::Run(config) = from_args(["--admission-limit".into(), "128".into()]).unwrap()
        else {
            panic!("expected server config")
        };
        assert_eq!(config.admission_limit, 128);
        assert!(from_args(["--admission-limit".into(), "0".into()]).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn relative_config_and_cli_paths_are_anchored_before_launching_the_child() {
        let cwd = std::env::temp_dir().join(format!("rpi-http-relative-{}", uuid::Uuid::new_v4()));
        let directory = cwd.join("config");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("http.json"), r#"{"project":"../workspace","sessionDir":"history","registry":"state/sessions.json","rpiBin":"./rpi","projectsRoot":"managed"}"#).unwrap();
        let Launch::Run(config) = from_args_at(["--config=config/http.json".into()], &cwd).unwrap()
        else {
            panic!("expected server config")
        };
        assert_eq!(config.project, directory.join("../workspace"));
        assert_eq!(config.session_dir, Some(directory.join("history")));
        assert_eq!(config.registry, directory.join("state/sessions.json"));
        assert_eq!(config.rpi_bin, directory.join("./rpi"));
        assert_eq!(config.projects_root, Some(directory.join("managed")));
        let Launch::Run(config) = from_args_at(
            [
                "--config=config/http.json".into(),
                "--project=workspace".into(),
                "--session-dir=history".into(),
                "--registry=state/sessions.json".into(),
                "--rpi-bin=./rpi".into(),
                "--projects-root=managed".into(),
                "--add-project=extra".into(),
            ],
            &cwd,
        )
        .unwrap() else {
            panic!("expected server config")
        };
        assert_eq!(config.project, cwd.join("workspace"));
        assert_eq!(config.session_dir, Some(cwd.join("history")));
        assert_eq!(config.registry, cwd.join("state/sessions.json"));
        assert_eq!(config.rpi_bin, cwd.join("./rpi"));
        assert_eq!(config.projects_root, Some(cwd.join("managed")));
        assert_eq!(config.projects, vec![cwd.join("extra")]);
        for executable in ["rpi", "custom-host"] {
            assert_eq!(
                resolve_executable(&cwd, Path::new(executable)),
                PathBuf::from(executable)
            );
        }
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn home_relative_paths_do_not_become_a_literal_tilde_directory() {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let Some(home) = home else {
            return;
        };
        let cwd = std::env::temp_dir().join(format!("rpi-http-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let Launch::Run(config) = from_args_at(
            [
                "--projects-root".into(),
                "~/Managed Projects".into(),
                "--add-project".into(),
                "~".into(),
            ],
            &cwd,
        )
        .unwrap() else {
            panic!("expected server config")
        };
        assert_eq!(config.projects_root, Some(home.join("Managed Projects")));
        assert_eq!(config.projects, vec![home.clone()]);
        // Only the current user's home is known; other `~user` forms stay literal.
        assert_eq!(resolve(&cwd, Path::new("~someone")), cwd.join("~someone"));
        std::fs::remove_dir_all(cwd).unwrap();
    }
}
