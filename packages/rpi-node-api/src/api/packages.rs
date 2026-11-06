//! Data-only distribution envelopes. Not an executable/native package manifest.
use super::{NativeKind, NativeScope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Metadata shipped with the application. Source text never grants permissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: NativeKind,
    pub title: String,
    pub description: String,
    pub author: String,
    pub origin: String,
    pub instructions: Vec<String>,
}

/// SHA-256 of `serde_json`'s compact serialization of (manifest, sources).
/// Sources are sorted by path. Bytes are UTF-8; no archive/script/binary execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageBundle {
    pub manifest: PackageManifest,
    pub sources: BTreeMap<String, String>,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageRef {
    pub id: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageOffer {
    pub manifest: PackageManifest,
    pub sha256: String,
    pub file_count: usize,
    pub size_bytes: usize,
    /// Current verified bytes on this execution node; NOT installed or loaded.
    pub acquired: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageCatalog {
    pub source: String,
    pub packages: Vec<PackageOffer>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageAcquisition {
    pub package: PackageRef,
    /// Node-local source for diagnostics, never a browser-local path.
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageInstall {
    pub package: PackageRef,
    pub scope: NativeScope,
    pub trusted: bool,
}
