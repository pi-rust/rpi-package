//! CLI overrides and optional global/project `.rpi/web.json` configuration.
use crate::AppConfig;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub const HELP: &str = "rpi-web [--config PATH] [--init]
       [--project PATH] [--rpi-bin PATH] [--bind IP] [--port N]
       [--add-project PATH]... [--projects-root PATH] [--assets PATH]
       [--session-dir PATH] [--registry PATH] [--version]

Reads ~/.rpi/web.json, then <startup-project>/.rpi/web.json.
--config loads only the specified file. CLI options override file settings.
--init creates a project config without overwriting an existing file.
Paths in JSON are relative to the config file; a bare rpiBin uses PATH.";

#[derive(Debug)]
pub enum Launch {
    Run(Box<AppConfig>),
    Help,
    Version,
    Initialized(PathBuf),
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projects: Option<Vec<PathBuf>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projects_root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assets: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpi_bin: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    registry: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_hosts: Option<Vec<String>>,
}

impl FileConfig {
    fn apply(self, config: &mut AppConfig, base: &Path) -> Result<(), String> {
        for (value, target) in [
            (self.project, &mut config.project),
            (self.registry, &mut config.registry),
        ] {
            if let Some(path) = value {
                *target = resolve_path(base, &path)?;
            }
        }
        for (value, target) in [
            (self.projects_root, &mut config.projects_root),
            (self.assets, &mut config.assets),
            (self.session_dir, &mut config.session_dir),
        ] {
            if let Some(path) = value {
                *target = Some(resolve_path(base, &path)?);
            }
        }
        if let Some(paths) = self.projects {
            config.projects = paths
                .iter()
                .map(|p| resolve_path(base, p))
                .collect::<Result<_, _>>()?;
        }
        if let Some(path) = self.rpi_bin {
            if path.as_os_str().is_empty() {
                return Err("rpiBin must not be empty".into());
            }
            config.rpi_bin = if path.components().count() == 1 && !path.is_absolute() {
                path
            } else {
                resolve_path(base, &path)?
            };
        }
        if let Some(bind) = self.bind {
            config.bind = bind;
        }
        if let Some(port) = self.port {
            config.port = port;
        }
        if let Some(hosts) = self.allowed_hosts {
            let hosts: Vec<_> = hosts.into_iter().map(|s| s.trim().to_string()).collect();
            if hosts.is_empty()
                || hosts.iter().any(|s| {
                    s.is_empty()
                        || s.contains(['/', '@', '*'])
                        || s.chars().any(char::is_whitespace)
                })
            {
                return Err("allowedHosts must contain Host names, with optional ports, without URLs or wildcards".into());
            }
            config.allowed_hosts = Some(hosts);
        }
        Ok(())
    }
}

fn resolve_path(base: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("paths must not be empty".into());
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Launch, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    configure(args, &cwd, home.as_deref())
}

fn configure(
    args: impl IntoIterator<Item = String>,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<Launch, String> {
    let mut args = args.into_iter();
    let mut cli = FileConfig::default();
    let mut additional_projects = Vec::new();
    let mut explicit = None;
    let mut init = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Launch::Help),
            "--version" | "-V" => return Ok(Launch::Version),
            "--init" => init = true,
            _ => {
                if ![
                    "--config",
                    "--project",
                    "--add-project",
                    "--projects-root",
                    "--assets",
                    "--rpi-bin",
                    "--bind",
                    "--port",
                    "--session-dir",
                    "--registry",
                ]
                .contains(&arg.as_str())
                {
                    return Err(format!("unknown argument: {arg}"));
                }
                let value = args
                    .next()
                    .filter(|s| !s.is_empty() && !s.starts_with("--"))
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                match arg.as_str() {
                    "--config" => explicit = Some(resolve_path(cwd, Path::new(&value))?),
                    "--project" => cli.project = Some(value.into()),
                    "--add-project" => {
                        additional_projects.push(resolve_path(cwd, Path::new(&value))?)
                    }
                    "--projects-root" => cli.projects_root = Some(value.into()),
                    "--assets" => cli.assets = Some(value.into()),
                    "--rpi-bin" => cli.rpi_bin = Some(value.into()),
                    "--bind" => cli.bind = Some(value),
                    "--port" => {
                        cli.port = Some(
                            value
                                .parse()
                                .map_err(|_| "--port requires a number from 0 to 65535")?,
                        )
                    }
                    "--session-dir" => cli.session_dir = Some(value.into()),
                    "--registry" => cli.registry = Some(value.into()),
                    _ => unreachable!(),
                }
            }
        }
    }
    let startup = resolve_path(cwd, cli.project.as_deref().unwrap_or(cwd))?;
    let local = startup.join(".rpi/web.json");
    if init {
        let file = explicit.clone().unwrap_or(local);
        let mut config = AppConfig {
            project: startup,
            ..AppConfig::default()
        };
        cli.apply(&mut config, cwd)?;
        config.projects.extend(additional_projects);
        let seed = FileConfig {
            project: Some(if explicit.is_none() {
                "..".into()
            } else {
                config.project
            }),
            projects: (!config.projects.is_empty()).then_some(config.projects),
            projects_root: config.projects_root,
            assets: config.assets,
            rpi_bin: Some(config.rpi_bin),
            bind: (config.bind != "127.0.0.1").then_some(config.bind),
            port: Some(config.port),
            session_dir: config.session_dir,
            registry: (config.registry != crate::registry::default_path())
                .then_some(config.registry),
            allowed_hosts: None,
        };
        std::fs::create_dir_all(file.parent().ok_or("config requires a parent directory")?)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let bytes = serde_json::to_vec_pretty(&seed).map_err(|e| e.to_string())?;
        let mut output = options.open(&file).map_err(|e| {
            format!(
                "{}: {e} (existing configs are never overwritten)",
                file.display()
            )
        })?;
        output
            .write_all(&bytes)
            .and_then(|_| output.write_all(b"\n"))
            .map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(Launch::Initialized(file));
    }
    let mut config = AppConfig {
        project: cwd.to_path_buf(),
        ..AppConfig::default()
    };
    if let Some(path) = explicit {
        load(&path, &mut config, true)?;
    } else {
        if let Some(home) = home {
            let global = home.join(".rpi/web.json");
            if global != local {
                load(&global, &mut config, false)?;
            }
        }
        load(&local, &mut config, false)?;
    }
    cli.apply(&mut config, cwd)?;
    config.projects.extend(additional_projects);
    Ok(Launch::Run(Box::new(config)))
}

fn load(path: &Path, config: &mut AppConfig, required: bool) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let file: FileConfig = serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.apply(
        config,
        path.parent().ok_or("config requires a parent directory")?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("rpi-web-config-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("work")).unwrap();
            std::fs::create_dir_all(root.join("home")).unwrap();
            Self(root)
        }
        fn write(&self, path: &str, text: &str) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn launch(&self, args: &[&str]) -> Result<Launch, String> {
            configure(
                args.iter().map(|s| s.to_string()),
                &self.0.join("work"),
                Some(&self.0.join("home")),
            )
        }
        fn run(&self, args: &[&str]) -> AppConfig {
            match self.launch(args).unwrap() {
                Launch::Run(config) => *config,
                other => panic!("expected launch, got {other:?}"),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert!(self.0.starts_with(std::env::temp_dir()));
            assert!(self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("rpi-web-config-"));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_file_keeps_legacy_defaults_and_cli_paths() {
        let f = Fixture::new();
        let config = f.run(&[]);
        assert_eq!(config.project, f.0.join("work"));
        assert_eq!(config.registry, crate::registry::default_path());
        assert_eq!(config.port, 3030);
        assert_eq!(config.rpi_bin, Path::new("rpi"));
        let config = f.run(&[
            "--project",
            "repo",
            "--assets",
            "ui",
            "--rpi-bin",
            "./bin/rpi",
            "--registry",
            "data/registry.json",
        ]);
        assert_eq!(config.project, f.0.join("work/repo"));
        assert_eq!(config.assets, Some(f.0.join("work/ui")));
        assert_eq!(config.rpi_bin, f.0.join("work/./bin/rpi"));
        assert_eq!(config.registry, f.0.join("work/data/registry.json"));
    }

    #[test]
    fn layers_files_and_cli_with_paths_bound_to_each_file() {
        let f = Fixture::new();
        f.write(
            "home/.rpi/web.json",
            r#"{"port":3100,"assets":"ui","rpiBin":"rpi","projects":["global-repo"]}"#,
        );
        f.write("work/.rpi/web.json", "\u{feff}{\"port\":3200,\"project\":\"..\",\"projectsRoot\":\"managed\",\"sessionDir\":\"history\",\"registry\":\"state/sessions.json\",\"allowedHosts\":[\" example.test \"],\"projects\":[\"local-repo\"]}");
        let local = f.run(&[]);
        assert_eq!(local.port, 3200);
        assert_eq!(local.project, f.0.join("work/.rpi/.."));
        assert_eq!(local.assets, Some(f.0.join("home/.rpi/ui")));
        assert_eq!(local.projects_root, Some(f.0.join("work/.rpi/managed")));
        assert_eq!(local.session_dir, Some(f.0.join("work/.rpi/history")));
        assert_eq!(local.registry, f.0.join("work/.rpi/state/sessions.json"));
        assert_eq!(local.allowed_hosts, Some(vec!["example.test".into()]));
        let cli = f.run(&[
            "--port",
            "3300",
            "--add-project",
            "extra",
            "--add-project",
            "other",
        ]);
        assert_eq!(cli.port, 3300);
        assert_eq!(
            cli.projects,
            [
                f.0.join("work/.rpi/local-repo"),
                f.0.join("work/extra"),
                f.0.join("work/other")
            ]
        );
    }

    #[test]
    fn project_flag_selects_its_config_independently_of_argument_order() {
        let f = Fixture::new();
        f.write("work/.rpi/web.json", "{broken}");
        f.write("work/repo/.rpi/web.json", r#"{"port":3456,"assets":"ui"}"#);
        for args in [
            ["--project", "repo", "--port", "4567"],
            ["--port", "4567", "--project", "repo"],
        ] {
            let config = f.run(&args);
            assert_eq!(config.port, 4567);
            assert_eq!(config.project, f.0.join("work/repo"));
            assert_eq!(config.assets, Some(f.0.join("work/repo/.rpi/ui")));
        }
    }

    #[test]
    fn explicit_config_is_independent_and_required() {
        let f = Fixture::new();
        f.write("home/.rpi/web.json", "{broken}");
        f.write("work/.rpi/web.json", "{broken}");
        f.write(
            "work/service/web.json",
            r#"{"project":"repo","rpiBin":"bin/rpi","projects":[]}"#,
        );
        let config = f.run(&["--config", "service/web.json"]);
        assert_eq!(config.project, f.0.join("work/service/repo"));
        assert_eq!(config.rpi_bin, f.0.join("work/service/bin/rpi"));
        assert!(config.projects.is_empty());
        assert!(f
            .launch(&["--config", "missing.json"])
            .unwrap_err()
            .contains("missing.json"));
        assert!(matches!(f.launch(&["--help"]).unwrap(), Launch::Help));
        assert!(matches!(f.launch(&["--version"]).unwrap(), Launch::Version));
    }

    #[test]
    fn init_is_portable_and_never_overwrites_configuration() {
        let f = Fixture::new();
        let path = f.0.join("work/.rpi/web.json");
        assert!(matches!(
            f.launch(&["--init", "--port", "3456"]).unwrap(),
            Launch::Initialized(_)
        ));
        let before = std::fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(value["project"], "..");
        assert_eq!(f.run(&[]).port, 3456);
        assert!(f
            .launch(&["--init"])
            .unwrap_err()
            .contains("never overwritten"));
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert!(matches!(
            f.launch(&["--config", "service.json", "--init", "--assets", "ui"])
                .unwrap(),
            Launch::Initialized(_)
        ));
        assert_eq!(
            f.run(&["--config", "service.json"]).assets,
            Some(f.0.join("work/ui"))
        );
    }

    #[test]
    fn malformed_configuration_and_arguments_fail_with_context() {
        let f = Fixture::new();
        for text in [
            r#"{"ports":3030}"#,
            r#"{"port":65536}"#,
            r#"{"project":""}"#,
            r#"{"allowedHosts":[]}"#,
            r#"{"allowedHosts":["http://example.test"]}"#,
        ] {
            f.write("work/.rpi/web.json", text);
            assert!(f.launch(&[]).unwrap_err().contains("web.json"));
        }
        for args in [
            vec!["--unknown"],
            vec!["--config"],
            vec!["--port", "--project", "repo"],
            vec!["--port", "abc"],
        ] {
            assert!(f.launch(&args).is_err());
        }
    }
}
