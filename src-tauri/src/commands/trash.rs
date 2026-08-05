//! Typed Trash IPC commands. Permanent deletion is intentionally exposed only through its
//! dedicated request type with exact-name confirmation.

use tauri::State;

use crate::{
    application::trash::{
        PermanentDeleteRequest, TrashEntryRequest, TrashEntryView, TrashExecuteRequest,
        TrashExecutionView, TrashPlanRequest, TrashPlanView, TrashRetentionSummary,
    },
    error::AppErrorView,
    runtime::AppRuntime,
};

macro_rules! blocking_trash_command {
    ($name:ident, $request:ty, $response:ty, $method:ident) => {
        #[tauri::command]
        #[specta::specta]
        pub async fn $name(
            runtime: State<'_, AppRuntime>,
            request: $request,
        ) -> Result<$response, AppErrorView> {
            let service = runtime.trash_service()?;
            runtime
                .run_blocking(move || service.$method(&request))
                .await?
                .map_err(Into::into)
        }
    };
}

blocking_trash_command!(
    trash_move_plan,
    TrashPlanRequest,
    TrashPlanView,
    plan_move_to_trash
);
blocking_trash_command!(
    trash_restore_plan,
    TrashEntryRequest,
    TrashPlanView,
    plan_restore
);
blocking_trash_command!(
    trash_permanent_delete_plan,
    PermanentDeleteRequest,
    TrashPlanView,
    plan_permanent_delete
);
blocking_trash_command!(
    trash_move_execute,
    TrashExecuteRequest,
    TrashExecutionView,
    execute_move_to_trash
);
blocking_trash_command!(
    trash_restore_execute,
    TrashExecuteRequest,
    TrashExecutionView,
    execute_restore
);
blocking_trash_command!(
    trash_permanent_delete_execute,
    TrashExecuteRequest,
    TrashExecutionView,
    execute_permanent_delete
);

#[tauri::command]
#[specta::specta]
pub async fn trash_entries_list(
    runtime: State<'_, AppRuntime>,
) -> Result<Vec<TrashEntryView>, AppErrorView> {
    let service = runtime.trash_service()?;
    runtime
        .run_blocking(move || service.entries())
        .await?
        .map_err(Into::into)
}

blocking_trash_command!(trash_entry_get, TrashEntryRequest, TrashEntryView, entry);

#[tauri::command]
#[specta::specta]
pub async fn trash_retention_summary(
    runtime: State<'_, AppRuntime>,
) -> Result<TrashRetentionSummary, AppErrorView> {
    let service = runtime.trash_service()?;
    runtime
        .run_blocking(move || service.retention_summary(crate::domain::UtcTimestamp::now()))
        .await?
        .map_err(Into::into)
}
