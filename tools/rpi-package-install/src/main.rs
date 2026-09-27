//! CLI entry point for the rpi extension installer/packager.

use std::path::PathBuf;
use std::process::ExitCode;

use rpi_package_install::{install, metadata, pack, platform};

const USAGE: &str = "\
rpi-package-install - install/pack the rpi extension cdylibs (cross-platform)

USAGE:
    rpi-package-install install [OPTIONS]
    rpi-package-install pack    [OPTIONS]
    rpi-package-install bundle  --archive <FILE> [OPTIONS]

COMMANDS:
    install   Copy the built extension libraries into the rpi extension dir
    pack      Stage a distributable bundle under dist/ and archive it
    bundle    Install from a bundle produced by `pack`

OPTIONS:
    --manifest-path <FILE>   Workspace Cargo.toml (auto-detected by walking up)
    --source-dir <DIR>       Where the built libraries live (default: target/release)
    --target-dir <DIR>       rpi extension dir (default: $RPI_CODING_AGENT_DIR
                             else ~/.rpi/agent/extensions)
    --archive <FILE>         Bundle to install (bundle command)
    --out <DIR>              Output directory for pack (default: dist/)
    --exclude <NAME>         Skip a package (repeatable, comma-separated allowed)
    --all                    Ignore catalog/packages.json and take every cdylib
    --no-archive             pack: stop after staging, do not create the archive
    -n, --dry-run            Show what would be copied, write nothing
    -q, --quiet              Only report errors
    -h, --help               Show this help
    -V, --version            Show the version

The extension list is derived from Cargo, never hardcoded:
  * in a workspace  -> cdylib members, filtered by catalog/packages.json
  * standalone      -> every *.dll / *.so / *.dylib beside the installer
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct Args {
    command: Option<String>,
    manifest: Option<PathBuf>,
    source_dir: Option<PathBuf>,
    target_dir: Option<PathBuf>,
    archive: Option<PathBuf>,
    out: Option<PathBuf>,
    exclude: Vec<String>,
    all: bool,
    no_archive: bool,
    dry_run: bool,
    quiet: bool,
}

fn run() -> anyhow::Result<()> {
    let args = parse(std::env::args().skip(1))?;
    let command = match args.command.as_deref() {
        Some(c) => c,
        None => {
            print!("{USAGE}");
            return Ok(());
        }
    };
    if matches!(command, "help" | "--help" | "-h") {
        print!("{USAGE}");
        return Ok(());
    }
    if matches!(command, "version" | "--version" | "-V") {
        println!("rpi-package-install {} ({})", env!("CARGO_PKG_VERSION"), platform::slug());
        return Ok(());
    }

    match command {
        "install" => {
            let manifest = resolve_manifest(&args)?;
            let installed = if manifest.is_none() && args.source_dir.is_none() {
                anyhow::bail!(
                    "no workspace found and no --source-dir given; nothing to install"
                );
            } else {
                install::run(&install::Options {
                    manifest,
                    source_dir: args.source_dir.clone(),
                    target_dir: args.target_dir.clone(),
                    all: args.all,
                    exclude: args.exclude.clone(),
                    dry_run: args.dry_run,
                    quiet: args.quiet,
                })?
            };
            if args.quiet {
                println!("installed {}", installed.len());
            }
            Ok(())
        }
        "pack" => {
            let manifest = resolve_manifest(&args)?.ok_or_else(|| {
                anyhow::anyhow!("`pack` must run inside the rpi-package workspace")
            })?;
            pack::run(&pack::Options {
                manifest: Some(manifest),
                source_dir: args.source_dir.clone(),
                out_dir: args.out.clone(),
                all: args.all,
                skip_archive: args.no_archive,
            })?;
            Ok(())
        }
        "bundle" => {
            let archive = args
                .archive
                .clone()
                .ok_or_else(|| anyhow::anyhow!("`bundle` requires --archive <FILE>"))?;
            let installed = pack::install_bundle(&archive, args.target_dir.clone())?;
            println!("installed {} extension(s)", installed.len());
            Ok(())
        }
        other => anyhow::bail!("unknown command `{other}` (try --help)"),
    }
}

/// `--manifest-path` wins, then an auto-detected workspace, else `None`
/// (standalone mode).
fn resolve_manifest(args: &Args) -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = &args.manifest {
        if !path.is_file() {
            anyhow::bail!("--manifest-path {} does not exist", path.display());
        }
        return Ok(Some(path.clone()));
    }
    let cwd = std::env::current_dir()?;
    Ok(metadata::find_manifest(&cwd))
}

fn parse(input: impl Iterator<Item = String>) -> anyhow::Result<Args> {
    let mut args = Args::default();
    let mut pending: Option<String> = None;
    for token in input {
        if let Some(flag) = pending.take() {
            apply(&mut args, &flag, &token)?;
            continue;
        }
        if let Some(rest) = token.strip_prefix("--") {
            if let Some((flag, value)) = rest.split_once('=') {
                apply(&mut args, &format!("--{flag}"), value)?;
                continue;
            }
        }
        match token.as_str() {
            "--all" => args.all = true,
            "--no-archive" => args.no_archive = true,
            "--dry-run" | "-n" => args.dry_run = true,
            "--quiet" | "-q" => args.quiet = true,
            "--help" | "-h" | "help" => args.command = Some("help".into()),
            "--version" | "-V" | "version" => args.command = Some("version".into()),
            t if t.starts_with('-') && t.len() > 1 => pending = Some(t.to_string()),
            t if args.command.is_none() => args.command = Some(t.to_string()),
            t => anyhow::bail!("unexpected argument `{t}`"),
        }
    }
    if let Some(flag) = pending {
        anyhow::bail!("{flag} requires a value");
    }
    Ok(args)
}

fn apply(args: &mut Args, flag: &str, value: &str) -> anyhow::Result<()> {
    match flag {
        "--manifest-path" => args.manifest = Some(PathBuf::from(value)),
        "--source-dir" => args.source_dir = Some(PathBuf::from(value)),
        "--target-dir" => args.target_dir = Some(PathBuf::from(value)),
        "--archive" => args.archive = Some(PathBuf::from(value)),
        "--out" => args.out = Some(PathBuf::from(value)),
        "--exclude" => args
            .exclude
            .extend(value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from)),
        other => anyhow::bail!("unknown option `{other}` (try --help)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(argv: &[&str]) -> Args {
        parse(argv.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn parses_command_and_flags() {
        let args = parse_ok(&["install", "--target-dir", "/tmp/x", "--all", "-n"]);
        assert_eq!(args.command.as_deref(), Some("install"));
        assert_eq!(args.target_dir, Some(PathBuf::from("/tmp/x")));
        assert!(args.all);
        assert!(args.dry_run);
    }

    #[test]
    fn supports_equals_and_repeated_exclude() {
        let args = parse_ok(&[
            "install",
            "--manifest-path=/ws/Cargo.toml",
            "--exclude",
            "rpi-voice",
            "--exclude=rpi-server,rpi-lens",
        ]);
        assert_eq!(args.manifest, Some(PathBuf::from("/ws/Cargo.toml")));
        assert_eq!(args.exclude, vec!["rpi-voice", "rpi-server", "rpi-lens"]);
    }

    #[test]
    fn rejects_unknown_options_and_dangling_values() {
        assert!(parse(["install".to_string(), "--nope".to_string()].into_iter()).is_err());
        assert!(parse(["install".to_string(), "--target-dir".to_string()].into_iter()).is_err());
        assert!(parse(["install".to_string(), "extra".to_string()].into_iter()).is_err());
    }

    #[test]
    fn bare_invocation_prints_usage() {
        let args = parse(Vec::<String>::new().into_iter()).unwrap();
        assert_eq!(args.command, None);
    }
}
