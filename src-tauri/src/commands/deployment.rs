//! Typed Target, deployment, health, and undeploy IPC use cases.

use tauri::State;

use crate::{
    application::deployment::{
        BatchDeploymentPlanRequest, BatchDeploymentPlanView, DeploymentHealthView,
        DeploymentIdRequest, DeploymentPage, DeploymentPlanRequest, DeploymentPlanView,
        DeploymentQuery, RegisterTargetRequest, TargetView, UndeployPlanRequest,
    },
    error::AppErrorView,
    runtime::AppRuntime,
};

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
