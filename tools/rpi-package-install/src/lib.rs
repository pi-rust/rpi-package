//! Cross-platform installer/packager for the rpi extension cdylibs.
//!
//! Replaces the old per-OS `scripts/install.ps1` / `scripts/install.sh` /
//! `scripts/pack.ps1` trio with a single code path that runs identically on
//! Windows, Linux and macOS (and in CI).
//!
//! The extension list is *derived*, never hardcoded:
//!
//! * in-repo  -> the workspace's `cdylib` members, filtered by
//!   `catalog/packages.json` when that file exists;
//! * standalone (release bundle) -> every `*.dll`/`*.so`/`*.dylib` next to the
//!   installer.

pub mod archive;
pub mod catalog;
pub mod install;
pub mod metadata;
pub mod pack;
pub mod platform;
