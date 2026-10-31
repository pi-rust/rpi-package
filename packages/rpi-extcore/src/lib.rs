//! Shared native-agent services for rpi protocol adapters.
//!
//! `rpi-extcore` is the protocol-neutral layer used by the Web and ACP
//! adapters. It owns native `rpi --mode rpc` child processes, request
//! correlation, prompt run settlement, event replay, bounded transcript
//! snapshots, workspace services and durable projection semantics.
//!
//! The crate deliberately has no Axum, ACP, frontend or plugin-ABI dependency.
//! Native session files remain owned by the native rpi host; this crate reads,
//! validates and bounds that data rather than creating a second transcript. The
//! durable service integration optionally uses the native `get_session_branch`
//! RPC plus the host `execute_command` bridge. The `rpi-durable` extension is
//! the only writer of durable business entries.
//! Hosts without those optional commands continue to support ordinary sessions.
//!
//! # Versioning
//!
//! The crate follows independent package SemVer within the workspace. The
//! current `0.1` line requires Rust 1.78 or newer. In the `0.x` series,
//! breaking public API changes use a minor-version bump; additive APIs and
//! compatible fixes use a patch-version bump unless a release note says
//! otherwise. Consumers should depend on the minor line, for example
//! `rpi-extcore = "0.1"`, rather than pinning an unpublished patch version.
#![forbid(unsafe_code)]
pub mod configuration;
pub mod dialogs;
pub mod durable;
pub mod events;
pub mod external_tools;
pub mod history;
pub mod host;
pub mod message;
pub mod process;
pub mod projects;
pub mod protocol;
pub mod registry;
pub mod reply;
pub mod run;
pub mod run_timing;
pub mod service;
pub mod session_catalog;
pub mod timing;
pub mod workspace;
