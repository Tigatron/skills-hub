use tauri::State;

use crate::{
    application::activity::{ActivityDetail, ActivityItem, ActivityQuery},
    error::AppErrorView,
    runtime::AppRuntime,
};

#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn activity_list(
    runtime: State<'_, AppRuntime>,
    query: ActivityQuery,
) -> Result<Vec<ActivityItem>, AppErrorView> {
    runtime.activity_service()?.list(query).map_err(Into::into)
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn activity_detail(
    runtime: State<'_, AppRuntime>,
    id: String,
) -> Result<ActivityDetail, AppErrorView> {
    runtime.activity_service()?.detail(&id).map_err(Into::into)
}
