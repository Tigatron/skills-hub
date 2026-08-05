use crate::{
    diagnostics::{DiagnosticsExport, DiagnosticsSaveResult, DiagnosticsStatus},
    error::AppErrorView,
    runtime::AppRuntime,
};
use serde::Deserialize;
use specta::Type;
use std::path::PathBuf;
use tauri::State;

#[allow(clippy::needless_pass_by_value)]
#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DebugRequest {
    pub enabled: bool,
}
#[derive(Debug, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaveRequest {
    pub export_id: String,
    pub expected_sha256: String,
    pub destination_path: String,
}
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub fn diagnostics_status(
    runtime: State<'_, AppRuntime>,
) -> Result<DiagnosticsStatus, AppErrorView> {
    runtime.diagnostics_status().map_err(Into::into)
}
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub fn diagnostics_debug_set(
    runtime: State<'_, AppRuntime>,
    request: DebugRequest,
) -> Result<DiagnosticsStatus, AppErrorView> {
    runtime
        .diagnostics_debug_set(request.enabled)
        .map_err(Into::into)
}
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub fn diagnostics_export_prepare(
    runtime: State<'_, AppRuntime>,
) -> Result<DiagnosticsExport, AppErrorView> {
    runtime.diagnostics_export_prepare().map_err(Into::into)
}
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub fn diagnostics_export_save(
    runtime: State<'_, AppRuntime>,
    request: SaveRequest,
) -> Result<DiagnosticsSaveResult, AppErrorView> {
    runtime
        .diagnostics_export_save(
            &request.export_id,
            &request.expected_sha256,
            &PathBuf::from(request.destination_path),
        )
        .map_err(Into::into)
}
