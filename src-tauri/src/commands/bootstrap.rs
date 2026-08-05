use serde::Serialize;
use specta::Type;
use std::path::PathBuf;
use tauri::State;

use crate::persistence::default_vault_path;
use crate::runtime::{RuntimeVaultSummary, StartupRecoveryReport};
use crate::{error::AppErrorView, runtime::AppRuntime};

#[derive(Debug, Clone, serde::Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InitializeVaultRequest {
    pub selected_directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultSummary {
    pub root_path: String,
    pub initialized: bool,
    pub vault_id: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatusView {
    pub initialized: bool,
    pub root_path: Option<String>,
    pub default_path: String,
    pub startup_recovery_completed: Option<bool>,
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn vault_initialize(
    request: InitializeVaultRequest,
    runtime: State<'_, AppRuntime>,
) -> Result<VaultSummary, AppErrorView> {
    let selected = request.selected_directory.map(PathBuf::from);
    runtime
        .initialize_vault(selected)
        .map(summary_view)
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn vault_status(runtime: State<'_, AppRuntime>) -> Result<VaultStatusView, AppErrorView> {
    vault_status_for_runtime(&runtime)
}

fn vault_status_for_runtime(runtime: &AppRuntime) -> Result<VaultStatusView, AppErrorView> {
    let home = runtime.home_path()?;
    let status = runtime.vault_status()?;
    Ok(VaultStatusView {
        initialized: status.is_some(),
        root_path: status
            .as_ref()
            .map(|status| status.summary.root_path.to_string_lossy().into_owned()),
        default_path: default_vault_path(&home).to_string_lossy().into_owned(),
        startup_recovery_completed: status.and_then(|status| status.startup_recovery_completed),
    })
}

fn summary_view(summary: RuntimeVaultSummary) -> VaultSummary {
    VaultSummary {
        root_path: summary.root_path.to_string_lossy().into_owned(),
        initialized: true,
        vault_id: summary.vault_id,
    }
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn startup_recovery_run(
    runtime: State<'_, AppRuntime>,
) -> Result<StartupRecoveryReport, AppErrorView> {
    runtime.startup_recovery_status().map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn startup_recovery_status(
    runtime: State<'_, AppRuntime>,
) -> Result<StartupRecoveryReport, AppErrorView> {
    runtime.startup_recovery_status().map_err(Into::into)
}

const CONTRACT_VERSION: u16 = 1;

/// Returns immutable build/runtime facts required to bootstrap the renderer.
#[tauri::command]
#[specta::specta]
pub async fn bootstrap_get_state(
    runtime: State<'_, AppRuntime>,
) -> Result<BootstrapState, AppErrorView> {
    runtime.run_blocking(|| ()).await?;
    let vault_status = vault_status_for_runtime(&runtime)?;

    Ok(BootstrapState {
        app_name: "Skills Hub",
        app_version: env!("CARGO_PKG_VERSION"),
        bundle_identifier: "com.terrylan.skillshub",
        contract_version: CONTRACT_VERSION,
        implementation_stage: "M0-009",
        vault_initialized: vault_status.initialized,
        vault_path: vault_status.root_path,
        runtime_status: RuntimeStatus::Ready,
        blocking_worker_limit: runtime.blocking_worker_limit(),
        platform: PlatformSummary {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            minimum_supported_os: "macOS 14 Sonoma",
        },
    })
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapState {
    app_name: &'static str,
    app_version: &'static str,
    bundle_identifier: &'static str,
    contract_version: u16,
    implementation_stage: &'static str,
    vault_initialized: bool,
    vault_path: Option<String>,
    runtime_status: RuntimeStatus,
    blocking_worker_limit: u8,
    platform: PlatformSummary,
}

#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "snake_case")]
enum RuntimeStatus {
    Ready,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
struct PlatformSummary {
    os: &'static str,
    arch: &'static str,
    minimum_supported_os: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_status_uses_stable_wire_value() {
        assert_eq!(
            serde_json::to_string(&RuntimeStatus::Ready).unwrap(),
            "\"ready\""
        );
    }
}
