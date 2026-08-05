use serde::Serialize;
use specta::Type;
use tauri::State;

use crate::runtime::StartupRecoveryReport;
use crate::{error::AppErrorView, runtime::AppRuntime};

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

    Ok(BootstrapState {
        app_name: "Skills Hub",
        app_version: env!("CARGO_PKG_VERSION"),
        bundle_identifier: "com.terrylan.skillshub",
        contract_version: CONTRACT_VERSION,
        implementation_stage: "M0-008",
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
