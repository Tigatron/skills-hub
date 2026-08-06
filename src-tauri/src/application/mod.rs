//! Use-case orchestration independent of Tauri and concrete persistence.

#[allow(dead_code)] // Runtime wiring is intentionally owned by the integration worker.
pub(crate) mod activity;
pub(crate) mod deployment;
#[cfg(test)]
pub(crate) mod reference_scale;
pub(crate) mod scanning;
pub(crate) mod takeover;
pub(crate) mod trash;
pub(crate) mod vault_lifecycle;
pub(crate) mod workspaces;
