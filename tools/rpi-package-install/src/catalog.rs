//! Reads `catalog/packages.json`, the curated list of extensions we ship.

use anyhow::{Context, Result};
use std::path::Path;

/// Package names listed in the catalog, in file order.
///
/// Returns `None` when the file does not exist, which means "ship everything
/// that built" (the standalone/bundle case).
pub fn load(path: &Path) -> Result<Option<Vec<String>>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {} as JSON", path.display()))?;
    let entries = value
        .as_array()
        .with_context(|| format!("{} should be a JSON array", path.display()))?;

    let mut names = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(|n| n.as_str())
            .with_context(|| format!("{}: entry without a string `name`", path.display()))?;
        names.push(name.to_string());
    }
    Ok(Some(names))
}

/// Default catalog location relative to the workspace root.
pub fn default_path(workspace_root: &Path) -> std::path::PathBuf {
    workspace_root.join("catalog").join("packages.json")
}
