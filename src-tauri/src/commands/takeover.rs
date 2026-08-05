//! Typed takeover, Operation, and Skill-detail IPC use cases.

use serde::Serialize;
use specta::Type;
use tauri::State;

use crate::{
    application::deployment::AnyOperationView,
    application::takeover::{
        ExecuteOperationRequest, KeepExternalRequest, KeepExternalResult, OperationCancelResult,
        OperationIdRequest, SkillDetail, SkillIdRequest, SkillPreviewRequest, TakeoverPlanRequest,
        TakeoverPlanView, TextPreview,
    },
    error::AppErrorView,
    operations::OperationKind,
    runtime::AppRuntime,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PlanExportView {
    pub operation_id: String,
    pub plan_digest: String,
    pub json: String,
}

#[tauri::command]
#[specta::specta]
pub async fn takeover_keep_external(
    runtime: State<'_, AppRuntime>,
    request: KeepExternalRequest,
) -> Result<KeepExternalResult, AppErrorView> {
    let service = runtime.takeover_service()?;
    runtime
        .run_blocking(move || service.keep_external(&request))
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn takeover_plan(
    runtime: State<'_, AppRuntime>,
    request: TakeoverPlanRequest,
) -> Result<TakeoverPlanView, AppErrorView> {
    let service = runtime.takeover_service()?;
    runtime
        .run_blocking(move || service.plan_takeover(request))
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn operation_execute(
    runtime: State<'_, AppRuntime>,
    request: ExecuteOperationRequest,
) -> Result<AnyOperationView, AppErrorView> {
    let deployment = runtime.deployment_service()?;
    let takeover = runtime.takeover_service()?;
    let result = runtime
        .run_blocking(
            move || match deployment.operation_kind(&request.operation_id)? {
                OperationKind::TakeOver => takeover
                    .execute_operation(&request.operation_id, &request.plan_digest)
                    .map(AnyOperationView::Takeover)
                    .map_err(AppErrorView::from),
                OperationKind::Deploy | OperationKind::Undeploy | OperationKind::Undo => deployment
                    .execute_any_operation(&request.operation_id, &request.plan_digest)
                    .map_err(AppErrorView::from),
                _ => Err(AppErrorView::from(
                    crate::application::deployment::DeploymentError::Journal(
                        "Operation kind is not executable in the M0 deployment slice".into(),
                    ),
                )),
            },
        )
        .await?;
    runtime.request_workspace_reconciliation(if result.is_ok() {
        crate::scanner::ReconcileReason::OperationFinished
    } else {
        crate::scanner::ReconcileReason::OperationRolledBack
    });
    result
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn operation_cancel(
    runtime: State<'_, AppRuntime>,
    request: OperationIdRequest,
) -> Result<OperationCancelResult, AppErrorView> {
    let deployment = runtime.deployment_service()?;
    match deployment.operation_kind(&request.operation_id)? {
        OperationKind::TakeOver => runtime
            .takeover_service()?
            .cancel(&request.operation_id)
            .map_err(AppErrorView::from),
        OperationKind::Deploy | OperationKind::Undeploy | OperationKind::Undo => {
            let cancellation_requested = deployment.cancel(&request.operation_id)?;
            Ok(OperationCancelResult {
                operation_id: request.operation_id,
                cancellation_requested,
            })
        }
        _ => Err(AppErrorView::from(
            crate::application::deployment::DeploymentError::Journal(
                "Operation kind is not cancelable in the M0 deployment slice".into(),
            ),
        )),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn operation_get(
    runtime: State<'_, AppRuntime>,
    request: OperationIdRequest,
) -> Result<AnyOperationView, AppErrorView> {
    let deployment = runtime.deployment_service()?;
    let takeover = runtime.takeover_service()?;
    runtime
        .run_blocking(
            move || match deployment.operation_kind(&request.operation_id)? {
                OperationKind::TakeOver => takeover
                    .get_operation(&request.operation_id)
                    .map(AnyOperationView::Takeover)
                    .map_err(AppErrorView::from),
                OperationKind::Deploy | OperationKind::Undeploy | OperationKind::Undo => deployment
                    .get_any_operation(&request.operation_id)
                    .map_err(AppErrorView::from),
                _ => Err(AppErrorView::from(
                    crate::application::deployment::DeploymentError::Journal(
                        "Operation kind has no M0 read model".into(),
                    ),
                )),
            },
        )
        .await?
}

/// Exports the validated persisted plan without authorizing or executing it.
#[tauri::command]
#[specta::specta]
pub async fn operation_plan_export(
    runtime: State<'_, AppRuntime>,
    request: OperationIdRequest,
) -> Result<PlanExportView, AppErrorView> {
    let service = runtime.deployment_service()?;
    let operation_id = request.operation_id;
    runtime
        .run_blocking(move || {
            let (plan_digest, json) = service.export_plan_json(&operation_id)?;
            Ok::<PlanExportView, crate::application::deployment::DeploymentError>(PlanExportView {
                operation_id,
                plan_digest,
                json,
            })
        })
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn skill_get(
    runtime: State<'_, AppRuntime>,
    request: SkillIdRequest,
) -> Result<SkillDetail, AppErrorView> {
    let service = runtime.takeover_service()?;
    runtime
        .run_blocking(move || service.skill_detail(&request.skill_id))
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn skill_preview_file(
    runtime: State<'_, AppRuntime>,
    request: SkillPreviewRequest,
) -> Result<TextPreview, AppErrorView> {
    let service = runtime.takeover_service()?;
    runtime
        .run_blocking(move || service.preview(&request.skill_id, &request.relative_path))
        .await?
        .map_err(AppErrorView::from)
}
