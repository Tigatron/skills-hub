//! Typed Workspace Root and manual-project command boundary.

use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_specta::Event;

use crate::{
    application::workspaces::{
        ManualProjectAddRequest, ManualProjectIdRequest, ManualProjectView, WorkspaceEventSink,
        WorkspaceProjectBatchEvent, WorkspaceRemoveResult, WorkspaceRootAddRequest,
        WorkspaceRootIdRequest, WorkspaceRootPauseRequest, WorkspaceRootUpdateRequest,
        WorkspaceRootView, WorkspaceScanResultView,
    },
    error::AppErrorView,
    runtime::AppRuntime,
};

#[tauri::command]
#[specta::specta]
pub async fn workspace_root_add(
    runtime: State<'_, AppRuntime>,
    request: WorkspaceRootAddRequest,
) -> Result<WorkspaceRootView, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.add(request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn workspace_root_update(
    runtime: State<'_, AppRuntime>,
    request: WorkspaceRootUpdateRequest,
) -> Result<WorkspaceRootView, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.update(request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn workspace_root_pause(
    runtime: State<'_, AppRuntime>,
    request: WorkspaceRootPauseRequest,
) -> Result<WorkspaceRootView, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.pause(request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn workspace_root_remove(
    runtime: State<'_, AppRuntime>,
    request: WorkspaceRootIdRequest,
) -> Result<WorkspaceRemoveResult, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.remove(request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn workspace_root_rescan(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
    request: WorkspaceRootIdRequest,
) -> Result<WorkspaceScanResultView, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || {
            service.rescan_with_events(request, Arc::new(TauriWorkspaceEvents(app)))
        })
        .await?
        .map_err(Into::into)
}

#[derive(Clone)]
struct TauriWorkspaceEvents(AppHandle);

impl WorkspaceEventSink for TauriWorkspaceEvents {
    fn project_batch(&self, event: WorkspaceProjectBatchEvent) {
        let _ = event.emit(&self.0);
    }
}

#[tauri::command]
#[specta::specta]
pub async fn workspace_roots_list(
    runtime: State<'_, AppRuntime>,
) -> Result<Vec<WorkspaceRootView>, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.list())
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn manual_project_add(
    runtime: State<'_, AppRuntime>,
    request: ManualProjectAddRequest,
) -> Result<ManualProjectView, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.add_manual_project(request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn manual_projects_list(
    runtime: State<'_, AppRuntime>,
) -> Result<Vec<ManualProjectView>, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.manual_projects())
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn manual_project_rescan(
    runtime: State<'_, AppRuntime>,
    request: ManualProjectIdRequest,
) -> Result<WorkspaceScanResultView, AppErrorView> {
    let service = runtime.workspace_service()?;
    runtime
        .run_blocking(move || service.rescan_manual_project(request))
        .await?
        .map_err(Into::into)
}
