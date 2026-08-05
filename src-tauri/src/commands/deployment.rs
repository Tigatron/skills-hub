//! Typed Target, deployment, health, and undeploy IPC use cases.

use serde::Serialize;
use specta::Type;
use tauri::State;

use crate::{
    application::deployment::{
        AdapterConfigureRequest, AdapterProjectTargetRegisterRequest, BatchDeploymentPlanRequest,
        BatchDeploymentPlanView, ConfiguredAdapterView, CustomTargetRegisterRequest,
        DeploymentHealthView, DeploymentIdRequest, DeploymentPage, DeploymentPlanRequest,
        DeploymentPlanView, DeploymentQuery, RegisterTargetRequest, TargetView,
        UndeployPlanRequest,
    },
    error::AppErrorView,
    runtime::AppRuntime,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OperationUndoPlanView {
    Available {
        plan: BatchDeploymentPlanView,
    },
    Conflict {
        detail: String,
        choices: Vec<String>,
    },
    Unavailable {
        detail: String,
    },
}

#[tauri::command]
#[specta::specta]
pub async fn batch_deployment_plan(
    runtime: State<'_, AppRuntime>,
    request: BatchDeploymentPlanRequest,
) -> Result<BatchDeploymentPlanView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.plan_batch_deployment(&request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn deployment_undo_plan(
    runtime: State<'_, AppRuntime>,
    request: crate::application::takeover::OperationIdRequest,
) -> Result<BatchDeploymentPlanView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.plan_undo(&request.operation_id))
        .await?
        .map_err(Into::into)
}

/// General undo entry point. M0 can invert successful batch deployments. Trash transitions and
/// other operation kinds are reported unavailable until they have exact inverse evidence.
#[tauri::command]
#[specta::specta]
pub async fn operation_undo_plan(
    runtime: State<'_, AppRuntime>,
    request: crate::application::takeover::OperationIdRequest,
) -> Result<OperationUndoPlanView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || match service.plan_undo(&request.operation_id) {
            Ok(plan) => Ok(OperationUndoPlanView::Available { plan }),
            Err(crate::application::deployment::DeploymentError::DriftBlocked(detail))
                if detail.contains("changed") =>
            {
                Ok(OperationUndoPlanView::Conflict {
                    detail,
                    choices: vec!["inspect".into(), "redeploy".into(), "restore".into()],
                })
            }
            Err(crate::application::deployment::DeploymentError::DriftBlocked(detail)) => {
                Ok(OperationUndoPlanView::Unavailable { detail })
            }
            Err(error) => Err(AppErrorView::from(error)),
        })
        .await?
}

#[tauri::command]
#[specta::specta]
pub async fn target_register_fixture(
    runtime: State<'_, AppRuntime>,
    request: RegisterTargetRequest,
) -> Result<TargetView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.register_target(&request))
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn adapters_configured_list(
    runtime: State<'_, AppRuntime>,
) -> Result<Vec<ConfiguredAdapterView>, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.adapters_configured_list())
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn adapter_configure(
    runtime: State<'_, AppRuntime>,
    request: AdapterConfigureRequest,
) -> Result<ConfiguredAdapterView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.configure_adapter(&request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn custom_target_register(
    runtime: State<'_, AppRuntime>,
    request: CustomTargetRegisterRequest,
) -> Result<TargetView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.register_custom_target(&request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn adapter_project_target_register(
    runtime: State<'_, AppRuntime>,
    request: AdapterProjectTargetRegisterRequest,
) -> Result<TargetView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.register_adapter_project_target(&request))
        .await?
        .map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
pub async fn targets_list(runtime: State<'_, AppRuntime>) -> Result<Vec<TargetView>, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.targets())
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn deployment_plan(
    runtime: State<'_, AppRuntime>,
    request: DeploymentPlanRequest,
) -> Result<DeploymentPlanView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.plan_deployment(&request))
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn undeploy_plan(
    runtime: State<'_, AppRuntime>,
    request: UndeployPlanRequest,
) -> Result<DeploymentPlanView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.plan_undeploy(&request))
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn deployment_verify(
    runtime: State<'_, AppRuntime>,
    request: DeploymentIdRequest,
) -> Result<DeploymentHealthView, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.verify(&request.deployment_id))
        .await?
        .map_err(AppErrorView::from)
}

#[tauri::command]
#[specta::specta]
pub async fn deployments_list(
    runtime: State<'_, AppRuntime>,
    query: DeploymentQuery,
) -> Result<DeploymentPage, AppErrorView> {
    let service = runtime.deployment_service()?;
    runtime
        .run_blocking(move || service.deployments_list(&query))
        .await?
        .map_err(AppErrorView::from)
}
