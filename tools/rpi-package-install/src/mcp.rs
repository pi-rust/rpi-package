//! Package the standalone `rpi-mcp-bridge` binary for MCP clients.
//!
//! An MCP client launches the bridge itself and speaks MCP on its stdin/stdout,
//! so the archive keeps one executable at its root exactly like the ACP bridge.
//! Unlike ACP there is no registry schema: the printed snippet is the client
//! configuration an MCP client needs.

use anyhow::Result;

use crate::standalone::{self, Spec};

/// Cargo bin target in `packages/rpi-mcp-server` (`[[bin]] name`).
pub const BINARY_STEM: &str = "rpi-mcp-bridge";

pub use crate::standalone::{Options, Outcome};

const SPEC: Spec = Spec {
    package: "rpi-mcp-server",
    binary_stem: BINARY_STEM,
    prefix: "rpi-mcp",
    readme: "packages/rpi-mcp-server/README.md",
};

pub fn run(opts: &Options) -> Result<Outcome> {
    let outcome = standalone::run(&SPEC, opts)?;
    if let Some(archive) = &outcome.archive {
        let file = archive
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        println!();
        println!("MCP client entry for this archive ({file}):");
        println!("  \"rpi\": {{");
        println!("    \"command\": \"/absolute/path/to/{BINARY_STEM}\",");
        println!("    \"args\": [\"--cwd\", \"/absolute/path/to/workspace\"]");
        println!("  }}");
    }
    Ok(outcome)
}
