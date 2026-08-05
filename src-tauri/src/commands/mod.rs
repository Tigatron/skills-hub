//! Thin Tauri IPC boundary.

#[allow(dead_code)] // Commands are declared here but intentionally not registered yet.
pub(crate) mod activity;
pub(crate) mod bootstrap;
pub(crate) mod deployment;
pub(crate) mod scanning;
pub(crate) mod takeover;
pub(crate) mod vault_lifecycle;
