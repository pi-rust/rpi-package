//! Package the standalone `rpi-acp-bridge` binary for the ACP Registry.
//!
//! `pack` produces the extension bundle an rpi host installs; this produces what
//! the ACP Registry's `distribution.binary` expects: a per-platform archive with
//! one agent binary at its root, plus a ready-to-paste `agent.json` stanza.
//!
//! The bridge speaks ACP on its own stdin/stdout and drives child
//! `rpi --mode rpc` processes, so an ACP client (Zed) can launch it directly —
//! no rpi host extension loading, no host `ownsStdio` change.

use anyhow::Result;

use crate::standalone::{self, Spec};

/// Cargo bin target in `packages/rpi-acp` (`[[bin]] name`).
pub const BINARY_STEM: &str = "rpi-acp-bridge";

pub use crate::standalone::{Options, Outcome};

const SPEC: Spec = Spec {
    package: "rpi-acp",
    binary_stem: BINARY_STEM,
    prefix: "rpi-acp",
    readme: "packages/rpi-acp/README.md",
};

pub fn run(opts: &Options) -> Result<Outcome> {
    let outcome = standalone::run(&SPEC, opts)?;
    if let (Some(archive), Some(sum)) = (&outcome.archive, &outcome.sha256) {
        let file = archive
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let slug = crate::platform::slug();
        println!();
        println!("registry distribution.binary entry for \"{slug}\":");
        println!("  \"{slug}\": {{");
        println!("    \"archive\": \"<release-url>/{file}\",");
        println!("    \"cmd\": \"./{BINARY_STEM}\",");
        println!("    \"args\": [],\n    \"sha256\": \"{sum}\"\n  }}");
    }
    Ok(outcome)
}
