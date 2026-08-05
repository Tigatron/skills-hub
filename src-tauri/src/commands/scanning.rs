use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_specta::Event;

use crate::{
    adapters::{AdapterDescriptorView, descriptors},
    application::scanning::{
        CancelResult, DomainInvalidated, JobRef, LibraryPage, LibraryQuery, ScanEventSink,
        ScanProgress, ScanRequest, ScanRunView, ScanSource,
    },
    error::AppErrorView,
    runtime::AppRuntime,
};

#[tauri::command]
#[specta::specta]
pub fn adapters_list() -> Vec<AdapterDescriptorView> {
    descriptors()
}

/// Starts independent read-only scans for every built-in global source.
#[tauri::command]
#[specta::specta]
pub async fn scan_all_global(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
) -> Result<Vec<JobRef>, AppErrorView> {
    let service = runtime.scanning_service()?;
    let mut jobs = Vec::new();
    for root in service
        .configured_global_roots()
        .map_err(AppErrorView::from)?
    {
        jobs.push(
            service
                .start(
                    ScanRequest {
                        source: ScanSource::ConfiguredGlobal(root.source_root_id),
                    },
                    Arc::new(TauriScanEvents(app.clone())),
                )
                .await
                .map_err(AppErrorView::from)?,
        );
    }
    Ok(jobs)
}

#[derive(Clone)]
struct TauriScanEvents(AppHandle);

impl ScanEventSink for TauriScanEvents {
    fn progress(&self, event: ScanProgress) {
        let _ = event.emit(&self.0);
    }

    fn invalidated(&self, event: DomainInvalidated) {
        let _ = event.emit(&self.0);
    }
}

/// Starts the configured Universal global scan without mutating its source root.
#[tauri::command]
#[specta::specta]
pub async fn scan_start(
    app: AppHandle,
    runtime: State<'_, AppRuntime>,
    request: ScanRequest,
) -> Result<JobRef, AppErrorView> {
    runtime
        .scanning_service()?
        .start(request, Arc::new(TauriScanEvents(app)))
        .await
        .map_err(AppErrorView::from)
}

/// Returns the authoritative state of one scan job.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn scan_get(
    runtime: State<'_, AppRuntime>,
    job_id: String,
) -> Result<ScanRunView, AppErrorView> {
    runtime
        .scanning_service()?
        .get(&job_id)
        .map_err(AppErrorView::from)
}

/// Requests cooperative cancellation at the next safe candidate boundary.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)]
pub fn scan_cancel(
    runtime: State<'_, AppRuntime>,
    job_id: String,
) -> Result<CancelResult, AppErrorView> {
    runtime
        .scanning_service()?
        .cancel(&job_id)
        .map_err(AppErrorView::from)
}

/// Lists paginated external Library items from the Rust-owned reconciliation model.
#[tauri::command]
#[specta::specta]
pub async fn library_list(
    runtime: State<'_, AppRuntime>,
    query: LibraryQuery,
) -> Result<LibraryPage, AppErrorView> {
    runtime
        .scanning_service()?
        .library_list(query)
        .await
        .map_err(AppErrorView::from)
}
