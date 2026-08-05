//! Typed Vault lifecycle command boundary.

use serde::Deserialize;
use specta::Type;
use tauri::State;

use crate::{
    application::vault_lifecycle::{
        IndexRebuildPlan, IndexRebuildResult, ObjectGcPhase, ObjectGcPlan, ObjectGcResult,
        ObjectGcSettingsSummary, OldVaultCleanupPlan, ReconcileResult, VaultRelocatePlan,
        VaultRelocateResult, VaultRepairPlan, VaultVerifyReport,
    },
    error::AppErrorView,
    runtime::AppRuntime,
};

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SkillLifecycleRequest {
    pub skill_id: String,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteLifecycleRequest {
    pub operation_id: String,
    pub plan_digest: String,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ObjectGcPlanRequest {
    pub phase: ObjectGcPhase,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultPathRequest {
    pub path: String,
}

#[tauri::command]
#[specta::specta]
pub async fn vault_reconcile_working(
    runtime: State<'_, AppRuntime>,
    request: SkillLifecycleRequest,
) -> Result<ReconcileResult, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let id = request.skill_id.parse().map_err(|_| AppErrorView {
        code: crate::error::AppErrorCode::InvalidInput,
        title: "Invalid Skill ID".into(),
        message: "The Skill identifier is not a valid UUID.".into(),
        retryable: false,
        recovery_action: None,
    })?;
    runtime
        .run_blocking(move || service.reconcile_external_edit(id))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_verify(
    runtime: State<'_, AppRuntime>,
) -> Result<VaultVerifyReport, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    runtime
        .run_blocking(move || service.verify())
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_repair_plan(
    runtime: State<'_, AppRuntime>,
) -> Result<VaultRepairPlan, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    runtime
        .run_blocking(move || service.plan_repair())
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_repair_execute(
    runtime: State<'_, AppRuntime>,
    request: ExecuteLifecycleRequest,
) -> Result<u32, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let operation_id = request.operation_id.parse().map_err(|_| AppErrorView {
        code: crate::error::AppErrorCode::InvalidInput,
        title: "Invalid Operation ID".into(),
        message: "The Operation identifier is not a valid UUID.".into(),
        retryable: false,
        recovery_action: None,
    })?;
    runtime
        .run_blocking(move || service.execute_repair(operation_id, &request.plan_digest))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_reveal_working(
    runtime: State<'_, AppRuntime>,
    request: SkillLifecycleRequest,
) -> Result<String, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let id = request.skill_id.parse().map_err(|_| AppErrorView {
        code: crate::error::AppErrorCode::InvalidInput,
        title: "Invalid Skill ID".into(),
        message: "The Skill identifier is not a valid UUID.".into(),
        retryable: false,
        recovery_action: None,
    })?;
    runtime
        .run_blocking(move || service.reveal_working(id))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_index_rebuild_plan(
    runtime: State<'_, AppRuntime>,
) -> Result<IndexRebuildPlan, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    runtime
        .run_blocking(move || service.plan_index_rebuild())
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_index_rebuild_execute(
    runtime: State<'_, AppRuntime>,
    request: ExecuteLifecycleRequest,
) -> Result<IndexRebuildResult, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let operation_id = request.operation_id.parse().map_err(|_| AppErrorView {
        code: crate::error::AppErrorCode::InvalidInput,
        title: "Invalid Operation ID".into(),
        message: "The Operation identifier is not a valid UUID.".into(),
        retryable: false,
        recovery_action: None,
    })?;
    let result = runtime
        .run_blocking(move || service.execute_index_rebuild(operation_id, &request.plan_digest))
        .await?
        .map_err(AppErrorView::from)?;
    runtime.enter_restart_required();
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_object_gc_plan(
    runtime: State<'_, AppRuntime>,
    request: ObjectGcPlanRequest,
) -> Result<ObjectGcPlan, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    runtime
        .run_blocking(move || service.plan_object_gc(request.phase))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_object_gc_execute(
    runtime: State<'_, AppRuntime>,
    request: ExecuteLifecycleRequest,
) -> Result<ObjectGcResult, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let operation_id = request.operation_id.parse().map_err(|_| AppErrorView {
        code: crate::error::AppErrorCode::InvalidInput,
        title: "Invalid Operation ID".into(),
        message: "The Operation identifier is not a valid UUID.".into(),
        retryable: false,
        recovery_action: None,
    })?;
    runtime
        .run_blocking(move || service.execute_object_gc(operation_id, &request.plan_digest))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_object_gc_settings(
    runtime: State<'_, AppRuntime>,
) -> Result<ObjectGcSettingsSummary, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    runtime
        .run_blocking(move || service.object_gc_settings_summary())
        .await
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_relocate_plan(
    runtime: State<'_, AppRuntime>,
    request: VaultPathRequest,
) -> Result<VaultRelocatePlan, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let path = std::path::PathBuf::from(request.path);
    runtime
        .run_blocking(move || service.plan_relocate(&path))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_relocate_execute(
    runtime: State<'_, AppRuntime>,
    request: ExecuteLifecycleRequest,
) -> Result<VaultRelocateResult, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let id = parse_operation_id(&request.operation_id)?;
    let result = runtime
        .run_blocking(move || service.execute_relocate(id, &request.plan_digest))
        .await?
        .map_err(AppErrorView::from)?;
    runtime.enter_restart_required();
    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_old_cleanup_plan(
    runtime: State<'_, AppRuntime>,
    request: VaultPathRequest,
) -> Result<OldVaultCleanupPlan, AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let path = std::path::PathBuf::from(request.path);
    runtime
        .run_blocking(move || service.plan_old_vault_cleanup(&path))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn vault_old_cleanup_execute(
    runtime: State<'_, AppRuntime>,
    request: ExecuteLifecycleRequest,
) -> Result<(), AppErrorView> {
    let service = runtime.vault_lifecycle_service()?;
    let id = parse_operation_id(&request.operation_id)?;
    runtime
        .run_blocking(move || service.execute_old_vault_cleanup(id, &request.plan_digest))
        .await?
        .map_err(Into::into)
}

fn parse_operation_id(value: &str) -> Result<crate::domain::OperationId, AppErrorView> {
    value.parse().map_err(|_| AppErrorView {
        code: crate::error::AppErrorCode::InvalidInput,
        title: "Invalid Operation ID".into(),
        message: "The Operation identifier is not a valid UUID.".into(),
        retryable: false,
        recovery_action: None,
    })
}
