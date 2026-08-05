use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;
use thiserror::Error;

use crate::{
    adapters::{GlobalAdapterRoot, global_roots},
    domain::{
        ActivityId, AdapterId, BundleDigest, ObservationId, ScanRunId, UtcTimestamp,
        normalized_collision_key, normalized_path_identity,
    },
    filesystem::BundleCaps,
    persistence::{
        ExternalObservationRecord, ObservationRecord, Repositories, RepositoryError,
        ScanErrorRecord, ScanReconciliation, ScanRunRecord,
    },
    runtime::{BlockingWorkError, BlockingWorkPool},
    scanner::{
        CancellationFlag, CoverageState, GlobalScanRequest, GlobalScanResult,
        ManagedLinkExpectation, ScanDiagnostic, ScanProgress as ScannerProgress, scan_global_root,
    },
};

const GLOBAL_SCOPE: &str = "global";
const SOURCE_ROOT_KIND: &str = "adapter_global";
const MAXIMUM_LIBRARY_PAGE_SIZE: u16 = 200;

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScanRequest {
    pub source: ScanSource,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScanSource {
    UniversalGlobal,
    ConfiguredGlobal(String),
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct JobRef {
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CancelResult {
    pub job_id: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ScanRunState {
    Queued,
    Running,
    Completed,
    CompletedWithErrors,
    Cancelled,
    Failed,
}

impl ScanRunState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::CompletedWithErrors => "completed_with_errors",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::CompletedWithErrors | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScanCoverageView {
    pub state: String,
    pub complete: bool,
    pub no_files_changed: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScanDiagnosticView {
    pub path: String,
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ScanRunView {
    pub job_id: String,
    pub adapter_id: String,
    pub source_root_id: String,
    pub source_name: String,
    pub display_root: String,
    pub state: ScanRunState,
    pub coverage: ScanCoverageView,
    pub completed_entries: u32,
    pub estimated_entries: u32,
    pub observation_count: u32,
    pub error_count: u32,
    pub errors: Vec<ScanDiagnosticView>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub job_id: String,
    pub phase: String,
    pub completed_entries: u32,
    pub estimated_entries: u32,
    pub current_display_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct DomainInvalidated {
    pub revision: u32,
    pub scopes: Vec<String>,
    pub ids: Vec<String>,
}

pub trait ScanEventSink: Send + Sync + 'static {
    fn progress(&self, event: ScanProgress);
    fn invalidated(&self, event: DomainInvalidated);
}

#[derive(Clone, Default)]
pub struct ScanJobs {
    jobs: Arc<Mutex<BTreeMap<ScanRunId, Arc<ScanJob>>>>,
    revision: Arc<AtomicU32>,
}

struct ScanJob {
    cancellation: CancellationFlag,
    view: Mutex<ScanRunView>,
}

#[derive(Clone)]
pub struct ScanningService {
    home: PathBuf,
    repositories: Repositories,
    blocking_work: BlockingWorkPool,
    jobs: ScanJobs,
}

impl ScanningService {
    #[must_use]
    pub fn new(
        home: PathBuf,
        repositories: Repositories,
        blocking_work: BlockingWorkPool,
        jobs: ScanJobs,
    ) -> Self {
        Self {
            home,
            repositories,
            blocking_work,
            jobs,
        }
    }

    /// Starts one read-only global scan and returns after its durable queued record exists.
    pub async fn start(
        &self,
        request: ScanRequest,
        events: Arc<dyn ScanEventSink>,
    ) -> Result<JobRef, ScanningServiceError> {
        let adapter = match request.source {
            ScanSource::UniversalGlobal => global_roots(&self.home).remove(0),
            ScanSource::ConfiguredGlobal(source_id) => self
                .configured_global_roots()?
                .into_iter()
                .find(|root| root.source_root_id == source_id)
                .ok_or(ScanningServiceError::UnknownSource)?,
        };
        // Resolve durable deployment evidence before publishing a queued job. If this read fails,
        // the command returns an error without leaving an in-memory or persisted job stuck forever.
        let managed_links = self
            .load_managed_links(&adapter.adapter_id, &adapter.root)
            .await?;
        let job_id = ScanRunId::generate();
        let started_at = UtcTimestamp::now();
        let view = ScanRunView {
            job_id: job_id.to_string(),
            adapter_id: adapter.adapter_id.to_string(),
            source_root_id: adapter.source_root_id.clone(),
            source_name: adapter.display_name.clone(),
            display_root: display_path(&adapter.root)?,
            state: ScanRunState::Queued,
            coverage: coverage_view(CoverageState::Partial),
            completed_entries: 0,
            estimated_entries: 0,
            observation_count: 0,
            error_count: 0,
            errors: Vec::new(),
            started_at: started_at.to_string(),
            completed_at: None,
        };
        let job = Arc::new(ScanJob {
            cancellation: CancellationFlag::default(),
            view: Mutex::new(view),
        });
        if let Some(existing) =
            self.jobs
                .insert_unless_active(job_id, Arc::clone(&job), &adapter.source_root_id)?
        {
            return Ok(existing);
        }

        let queued = scan_run_record(
            job_id,
            &adapter.source_root_id,
            ScanRunState::Queued,
            CoverageState::Partial,
            started_at,
            None,
            ScanRunCounts::default(),
        );
        if let Err(error) = self
            .run_repository(move |repositories| repositories.upsert_scan_run(queued))
            .await
        {
            self.jobs.remove(job_id)?;
            return Err(error);
        }

        let scan_request = GlobalScanRequest {
            adapter_id: adapter.adapter_id,
            source_root_id: adapter.source_root_id.clone(),
            root: adapter.root,
            caps: BundleCaps::default(),
            managed_links,
        };
        let service = self.clone();
        tokio::spawn(async move {
            service
                .execute_job(job_id, started_at, job, scan_request, events)
                .await;
        });

        Ok(JobRef {
            job_id: job_id.to_string(),
        })
    }

    /// Resolves only enabled, persisted global sources. Project custom targets are intentionally
    /// left to manual-project scanning because their observations are not global.
    pub fn configured_global_roots(&self) -> Result<Vec<GlobalAdapterRoot>, ScanningServiceError> {
        let configurations = self
            .repositories
            .adapter_configurations()
            .map_err(ScanningServiceError::Repository)?;
        let mut roots = Vec::new();
        for descriptor in crate::adapters::DESCRIPTORS {
            let configuration = configurations
                .iter()
                .find(|row| row.adapter_name == descriptor.name);
            if configuration.is_some_and(|row| !row.enabled) {
                continue;
            }
            roots.push(descriptor.global_root(&self.home));
            if let Some(path) = configuration.and_then(|row| row.global_override_path.clone()) {
                roots.push(GlobalAdapterRoot {
                    adapter_id: descriptor.id(),
                    source_root_id: format!("{}:global-override", descriptor.id()),
                    display_name: format!("{} override", descriptor.display_name),
                    root: path,
                });
            }
        }
        for target in self
            .repositories
            .targets(500)
            .map_err(ScanningServiceError::Repository)?
            .into_iter()
            .filter(|target| target.is_custom && target.scope == "global")
        {
            roots.push(GlobalAdapterRoot {
                adapter_id: target.adapter_id,
                source_root_id: format!("custom-directory@1:target:{}", target.id),
                display_name: self
                    .repositories
                    .target_registration_metadata(target.id)
                    .map_err(ScanningServiceError::Repository)?
                    .map_or_else(
                        || "Custom directory".into(),
                        |metadata| metadata.display_name,
                    ),
                root: target.root_path,
            });
        }
        Ok(roots)
    }

    pub fn get(&self, job_id: &str) -> Result<ScanRunView, ScanningServiceError> {
        let job_id = job_id
            .parse::<ScanRunId>()
            .map_err(|_| ScanningServiceError::JobNotFound)?;
        let job = self.jobs.get(job_id)?;
        let view = job
            .view
            .lock()
            .map_err(|_| ScanningServiceError::StatePoisoned)?
            .clone();
        Ok(view)
    }

    pub fn cancel(&self, job_id: &str) -> Result<CancelResult, ScanningServiceError> {
        let job_id = job_id
            .parse::<ScanRunId>()
            .map_err(|_| ScanningServiceError::JobNotFound)?;
        let job = self.jobs.get(job_id)?;
        let accepted = {
            let view = job
                .view
                .lock()
                .map_err(|_| ScanningServiceError::StatePoisoned)?;
            !view.state.is_terminal()
        };
        if accepted {
            job.cancellation.cancel();
        }
        Ok(CancelResult {
            job_id: job_id.to_string(),
            accepted,
        })
    }

    pub async fn library_list(
        &self,
        query: LibraryQuery,
    ) -> Result<LibraryPage, ScanningServiceError> {
        if query.limit == 0 || query.limit > MAXIMUM_LIBRARY_PAGE_SIZE {
            return Err(ScanningServiceError::InvalidLibraryLimit {
                maximum: MAXIMUM_LIBRARY_PAGE_SIZE,
            });
        }
        let (records, skills, deployments) = self
            .run_repository(|repositories| {
                Ok((
                    repositories.external_observations()?,
                    repositories.skills()?,
                    repositories.all_deployments()?,
                ))
            })
            .await?;
        Ok(build_library_page(records, skills, &deployments, &query))
    }

    async fn load_managed_links(
        &self,
        adapter_id: &AdapterId,
        root: &Path,
    ) -> Result<BTreeMap<String, ManagedLinkExpectation>, ScanningServiceError> {
        let adapter_id = adapter_id.clone();
        let records = self
            .run_repository(move |repositories| {
                repositories.managed_link_records(adapter_id, GLOBAL_SCOPE.to_owned())
            })
            .await?;
        let location_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut links = BTreeMap::new();
        for record in records {
            let Some(record_parent) = record.target_path.parent() else {
                continue;
            };
            let record_parent = record_parent
                .canonicalize()
                .unwrap_or_else(|_| record_parent.to_path_buf());
            if record_parent != location_root {
                continue;
            }
            let Some(name) = record
                .target_path
                .file_name()
                .and_then(|value| value.to_str())
            else {
                continue;
            };
            let Some(location) = location_root
                .join(name)
                .to_str()
                .map(normalized_path_identity)
            else {
                continue;
            };
            links.insert(
                location,
                ManagedLinkExpectation {
                    skill_id: record.skill_id,
                    raw_target: record.expected_target.clone(),
                    resolved_target: record.expected_target,
                },
            );
        }
        Ok(links)
    }

    async fn execute_job(
        &self,
        job_id: ScanRunId,
        started_at: UtcTimestamp,
        job: Arc<ScanJob>,
        request: GlobalScanRequest,
        events: Arc<dyn ScanEventSink>,
    ) {
        let source_root_id = request.source_root_id.clone();
        if let Err(error) = set_job_running(&job, &events) {
            self.fail_job(
                job_id,
                started_at,
                source_root_id,
                &job,
                &events,
                &error.to_string(),
            )
            .await;
            return;
        }
        let running = scan_run_record(
            job_id,
            &request.source_root_id,
            ScanRunState::Running,
            CoverageState::Partial,
            started_at,
            None,
            ScanRunCounts::default(),
        );
        if let Err(error) = self
            .run_repository(move |repositories| repositories.upsert_scan_run(running))
            .await
        {
            self.fail_job(
                job_id,
                started_at,
                source_root_id,
                &job,
                &events,
                &error.to_string(),
            )
            .await;
            return;
        }

        let callback_job = Arc::clone(&job);
        let callback_events = Arc::clone(&events);
        let callback_id = job_id.to_string();
        let cancellation = job.cancellation.clone();
        let result = self
            .blocking_work
            .run(move || {
                scan_global_root(&request, &cancellation, |progress| {
                    update_progress(&callback_job, &callback_events, &callback_id, &progress);
                })
            })
            .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.fail_job(
                    job_id,
                    started_at,
                    source_root_id,
                    &job,
                    &events,
                    &error.to_string(),
                )
                .await;
                return;
            }
        };
        let completed_at = UtcTimestamp::now();
        let state = terminal_state(&result);
        let reconciliation = reconciliation(job_id, started_at, completed_at, state, &result);
        if let Err(error) = self
            .run_repository(move |repositories| repositories.reconcile_scan(reconciliation))
            .await
        {
            self.fail_job(
                job_id,
                started_at,
                source_root_id,
                &job,
                &events,
                &error.to_string(),
            )
            .await;
            return;
        }
        let _ = set_job_terminal(&job, state, completed_at, &result);
        events.progress(progress_event(&job, "terminal"));
        events.invalidated(DomainInvalidated {
            revision: self.jobs.next_revision(),
            scopes: vec!["scan".to_owned(), "library".to_owned()],
            ids: vec![job_id.to_string()],
        });
    }

    async fn fail_job(
        &self,
        job_id: ScanRunId,
        started_at: UtcTimestamp,
        source_root_id: String,
        job: &ScanJob,
        events: &Arc<dyn ScanEventSink>,
        summary: &str,
    ) {
        let completed_at = UtcTimestamp::now();
        let _ = set_job_failed(job, events, completed_at, summary);
        let counts = job
            .view
            .lock()
            .map(|view| ScanRunCounts {
                completed_entries: usize::try_from(view.completed_entries).unwrap_or(usize::MAX),
                observation_count: usize::try_from(view.observation_count).unwrap_or(usize::MAX),
                error_count: usize::try_from(view.error_count).unwrap_or(usize::MAX),
            })
            .unwrap_or(ScanRunCounts {
                error_count: 1,
                ..ScanRunCounts::default()
            });
        let failed = scan_run_record(
            job_id,
            &source_root_id,
            ScanRunState::Failed,
            CoverageState::Partial,
            started_at,
            Some(completed_at),
            counts,
        );
        let _ = self
            .run_repository(move |repositories| repositories.upsert_scan_run(failed))
            .await;
        events.invalidated(DomainInvalidated {
            revision: self.jobs.next_revision(),
            scopes: vec!["scan".to_owned()],
            ids: vec![job_id.to_string()],
        });
    }

    async fn run_repository<T, F>(&self, work: F) -> Result<T, ScanningServiceError>
    where
        T: Send + 'static,
        F: FnOnce(&Repositories) -> Result<T, RepositoryError> + Send + 'static,
    {
        let repositories = self.repositories.clone();
        self.blocking_work
            .run(move || work(&repositories))
            .await?
            .map_err(ScanningServiceError::Repository)
    }
}

impl ScanJobs {
    fn next_revision(&self) -> u32 {
        self.revision
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |revision| {
                Some(revision.saturating_add(1))
            })
            .unwrap_or(u32::MAX)
            .saturating_add(1)
    }

    fn insert_unless_active(
        &self,
        id: ScanRunId,
        job: Arc<ScanJob>,
        source_root_id: &str,
    ) -> Result<Option<JobRef>, ScanningServiceError> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| ScanningServiceError::StatePoisoned)?;
        for active in jobs.values() {
            let view = active
                .view
                .lock()
                .map_err(|_| ScanningServiceError::StatePoisoned)?;
            if view.source_root_id == source_root_id && !view.state.is_terminal() {
                return Ok(Some(JobRef {
                    job_id: view.job_id.clone(),
                }));
            }
        }
        jobs.insert(id, job);
        Ok(None)
    }

    fn get(&self, id: ScanRunId) -> Result<Arc<ScanJob>, ScanningServiceError> {
        self.jobs
            .lock()
            .map_err(|_| ScanningServiceError::StatePoisoned)?
            .get(&id)
            .cloned()
            .ok_or(ScanningServiceError::JobNotFound)
    }

    fn remove(&self, id: ScanRunId) -> Result<(), ScanningServiceError> {
        self.jobs
            .lock()
            .map_err(|_| ScanningServiceError::StatePoisoned)?
            .remove(&id);
        Ok(())
    }
}

fn set_job_running(
    job: &ScanJob,
    events: &Arc<dyn ScanEventSink>,
) -> Result<(), ScanningServiceError> {
    {
        let mut view = job
            .view
            .lock()
            .map_err(|_| ScanningServiceError::StatePoisoned)?;
        view.state = ScanRunState::Running;
    }
    events.progress(progress_event(job, "enumerating"));
    Ok(())
}

fn update_progress(
    job: &ScanJob,
    events: &Arc<dyn ScanEventSink>,
    job_id: &str,
    progress: &ScannerProgress,
) {
    let Ok(mut view) = job.view.lock() else {
        return;
    };
    view.completed_entries = count(progress.completed_entries);
    view.estimated_entries = count(progress.estimated_entries);
    drop(view);
    events.progress(ScanProgress {
        job_id: job_id.to_owned(),
        phase: "hashing".to_owned(),
        completed_entries: count(progress.completed_entries),
        estimated_entries: count(progress.estimated_entries),
        current_display_path: progress
            .current_path
            .as_deref()
            .and_then(Path::to_str)
            .map(str::to_owned),
    });
}

fn set_job_terminal(
    job: &ScanJob,
    state: ScanRunState,
    completed_at: UtcTimestamp,
    result: &GlobalScanResult,
) -> Result<(), ScanningServiceError> {
    let mut view = job
        .view
        .lock()
        .map_err(|_| ScanningServiceError::StatePoisoned)?;
    view.state = state;
    view.coverage = coverage_view(result.coverage);
    view.completed_entries = count(result.completed_entries);
    view.estimated_entries = count(result.estimated_entries);
    view.observation_count = count(result.observations.len());
    view.error_count = count(result.diagnostics.len());
    view.errors = result
        .diagnostics
        .iter()
        .take(50)
        .map(diagnostic_view)
        .collect();
    view.completed_at = Some(completed_at.to_string());
    Ok(())
}

fn set_job_failed(
    job: &ScanJob,
    events: &Arc<dyn ScanEventSink>,
    completed_at: UtcTimestamp,
    summary: &str,
) -> Result<(), ScanningServiceError> {
    {
        let mut view = job
            .view
            .lock()
            .map_err(|_| ScanningServiceError::StatePoisoned)?;
        view.state = ScanRunState::Failed;
        view.coverage = coverage_view(CoverageState::Partial);
        view.error_count = view.error_count.saturating_add(1);
        let display_root = view.display_root.clone();
        view.errors.push(ScanDiagnosticView {
            path: display_root,
            code: "scan_failed".to_owned(),
            summary: summary.to_owned(),
        });
        view.completed_at = Some(completed_at.to_string());
    }
    events.progress(progress_event(job, "terminal"));
    Ok(())
}

fn progress_event(job: &ScanJob, phase: &str) -> ScanProgress {
    let Ok(view) = job.view.lock() else {
        return ScanProgress {
            job_id: String::new(),
            phase: phase.to_owned(),
            completed_entries: 0,
            estimated_entries: 0,
            current_display_path: None,
        };
    };
    ScanProgress {
        job_id: view.job_id.clone(),
        phase: phase.to_owned(),
        completed_entries: view.completed_entries,
        estimated_entries: view.estimated_entries,
        current_display_path: None,
    }
}

fn terminal_state(result: &GlobalScanResult) -> ScanRunState {
    if result.coverage == CoverageState::Cancelled {
        ScanRunState::Cancelled
    } else if result.coverage.is_complete() && result.diagnostics.is_empty() {
        ScanRunState::Completed
    } else {
        ScanRunState::CompletedWithErrors
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ScanRunCounts {
    completed_entries: usize,
    observation_count: usize,
    error_count: usize,
}

fn scan_run_record(
    id: ScanRunId,
    source_root_id: &str,
    state: ScanRunState,
    coverage: CoverageState,
    started_at: UtcTimestamp,
    completed_at: Option<UtcTimestamp>,
    counts: ScanRunCounts,
) -> ScanRunRecord {
    ScanRunRecord {
        id,
        root_kind: SOURCE_ROOT_KIND.to_owned(),
        root_id: Some(source_root_id.to_owned()),
        scope: GLOBAL_SCOPE.to_owned(),
        state: state.as_str().to_owned(),
        coverage: serde_json::json!({
            "state": coverage_text(coverage),
            "complete": coverage.is_complete(),
            "completedEntries": counts.completed_entries,
            "observationCount": counts.observation_count,
            "errorCount": counts.error_count,
            "noFilesChanged": true
        }),
        started_at,
        completed_at,
    }
}

fn reconciliation(
    job_id: ScanRunId,
    started_at: UtcTimestamp,
    completed_at: UtcTimestamp,
    state: ScanRunState,
    result: &GlobalScanResult,
) -> ScanReconciliation {
    let successful_run_id = result.coverage.is_complete().then_some(job_id);
    let observations = result
        .observations
        .iter()
        .map(|observation| ObservationRecord {
            id: ObservationId::generate(),
            skill_id: observation.skill_id,
            adapter_id: observation.adapter_id.clone(),
            scope: GLOBAL_SCOPE.to_owned(),
            project_id: None,
            source_root_kind: SOURCE_ROOT_KIND.to_owned(),
            source_root_id: observation.source_root_id.clone(),
            display_path: observation.display_path.clone(),
            normalized_path: observation.normalized_path.clone(),
            canonical_path: observation.canonical_path.clone(),
            deployment_name: observation.deployment_name.clone(),
            digest: observation.digest,
            status: observation.status.as_str().to_owned(),
            error_code: observation
                .error
                .as_ref()
                .map(|error| error.code.to_owned()),
            error_summary: observation
                .error
                .as_ref()
                .map(|error| error.summary.clone()),
            last_successful_run_id: successful_run_id,
            first_seen_at: completed_at,
            observed_at: completed_at,
            stale_at: None,
        })
        .collect();
    let errors = result
        .diagnostics
        .iter()
        .map(|diagnostic| ScanErrorRecord {
            scan_run_id: job_id,
            path: diagnostic.path.clone(),
            error_code: diagnostic.code.to_owned(),
            summary: diagnostic.summary.clone(),
        })
        .collect();
    ScanReconciliation {
        run: scan_run_record(
            job_id,
            &result.source_root_id,
            state,
            result.coverage,
            started_at,
            Some(completed_at),
            ScanRunCounts {
                completed_entries: result.completed_entries,
                observation_count: result.observations.len(),
                error_count: result.diagnostics.len(),
            },
        ),
        adapter_id: result.adapter_id.clone(),
        scope: GLOBAL_SCOPE.to_owned(),
        source_root_kind: SOURCE_ROOT_KIND.to_owned(),
        source_root_id: result.source_root_id.clone(),
        observations,
        errors,
        coverage_complete: result.coverage.is_complete(),
        activity: crate::persistence::ActivityRecord {
            id: ActivityId::generate(),
            operation_id: None,
            kind: "scan".to_owned(),
            state: state.as_str().to_owned(),
            outcome: None,
            summary: format!(
                "Scan finished with {} diagnostic(s)",
                result.diagnostics.len()
            ),
            details: serde_json::json!({
                "scanRunId": job_id,
                "coverage": coverage_text(result.coverage),
                "observationCount": result.observations.len(),
                "diagnosticCount": result.diagnostics.len(),
                "diagnostics": result.diagnostics.iter().map(|diagnostic| serde_json::json!({
                    "path": diagnostic.path, "errorCode": diagnostic.code, "summary": diagnostic.summary
                })).collect::<Vec<_>>()
            }),
            started_at,
            completed_at: Some(completed_at),
        },
    }
}

fn coverage_view(state: CoverageState) -> ScanCoverageView {
    ScanCoverageView {
        state: coverage_text(state).to_owned(),
        complete: state.is_complete(),
        no_files_changed: true,
    }
}

fn coverage_text(state: CoverageState) -> &'static str {
    match state {
        CoverageState::Complete => "complete",
        CoverageState::Missing => "missing",
        CoverageState::Inaccessible => "inaccessible",
        CoverageState::InvalidRoot => "invalid_root",
        CoverageState::Partial => "partial",
        CoverageState::Cancelled => "cancelled",
    }
}

fn diagnostic_view(diagnostic: &ScanDiagnostic) -> ScanDiagnosticView {
    ScanDiagnosticView {
        path: diagnostic
            .path
            .to_str()
            .unwrap_or("Unsupported path")
            .to_owned(),
        code: diagnostic.code.to_owned(),
        summary: diagnostic.summary.clone(),
    }
}

fn display_path(path: &Path) -> Result<String, ScanningServiceError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(ScanningServiceError::UnsupportedPath)
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryQuery {
    pub offset: u32,
    pub limit: u16,
    pub search: Option<String>,
    pub filter: LibraryFilter,
}

#[derive(Debug, Clone, Copy, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LibraryFilter {
    All,
    Verified,
    Errors,
    Conflicts,
    Duplicates,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPage {
    pub items: Vec<LibraryItem>,
    pub total: u32,
    pub offset: u32,
    pub limit: u16,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryItem {
    pub id: String,
    pub skill_id: Option<String>,
    pub display_name: String,
    pub deployment_name: String,
    pub ownership: LibraryOwnership,
    pub source_summary: String,
    pub locations: Vec<LibraryLocation>,
    pub digest: Option<String>,
    pub validation: LibraryValidation,
    pub duplicate_summary: DuplicateSummary,
    pub deployment_count: u32,
    pub working_location: Option<String>,
    pub changed_at: String,
    pub next_actions: Vec<LibraryAction>,
}

#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LibraryOwnership {
    External,
    Vaulted,
    Managed,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryLocation {
    pub observation_id: String,
    pub adapter_id: String,
    pub source_root_id: String,
    pub path: String,
    pub status: String,
    pub error: Option<LibraryErrorView>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LibraryErrorView {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LibraryValidation {
    Verified,
    Error,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateSummary {
    pub exact_duplicate_locations: u32,
    pub name_conflicts: u32,
    pub probable_duplicates_or_renames: u32,
    pub unverified: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LibraryAction {
    KeepExternal,
    AddToVault,
    AddAndManage,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ExternalGroupKey {
    Verified {
        name_key: String,
        digest: BundleDigest,
    },
    Unverified(ObservationId),
}

struct ExternalGroup {
    name_key: String,
    digest: Option<BundleDigest>,
    records: Vec<ExternalObservationRecord>,
}

fn build_library_page(
    records: Vec<ExternalObservationRecord>,
    skills: Vec<crate::persistence::SkillRecord>,
    deployments: &[crate::persistence::DeploymentRecord],
    query: &LibraryQuery,
) -> LibraryPage {
    let mut grouped = BTreeMap::<ExternalGroupKey, ExternalGroup>::new();
    for record in records {
        let name_key = record.deployment_name.collision_key().to_owned();
        let key = record.digest.map_or_else(
            || ExternalGroupKey::Unverified(record.id),
            |digest| ExternalGroupKey::Verified {
                name_key: name_key.clone(),
                digest,
            },
        );
        grouped
            .entry(key)
            .or_insert_with(|| ExternalGroup {
                name_key,
                digest: record.digest,
                records: Vec::new(),
            })
            .records
            .push(record);
    }

    let mut name_digests = BTreeMap::<String, BTreeSet<BundleDigest>>::new();
    let mut digest_names = BTreeMap::<BundleDigest, BTreeSet<String>>::new();
    for group in grouped.values() {
        if let Some(digest) = group.digest {
            name_digests
                .entry(group.name_key.clone())
                .or_default()
                .insert(digest);
            digest_names
                .entry(digest)
                .or_default()
                .insert(group.name_key.clone());
        }
    }

    let search = query.search.as_deref().map(normalized_collision_key);
    let mut items = grouped
        .into_values()
        .map(|group| library_item(group, &name_digests, &digest_names))
        .filter(|item| matches_search(item, search.as_deref()))
        .filter(|item| matches_filter(item, query.filter))
        .collect::<Vec<_>>();
    items.extend(
        managed_library_items(skills, deployments)
            .into_iter()
            .filter(|item| matches_search(item, search.as_deref()))
            .filter(|item| matches_filter(item, query.filter)),
    );
    items.sort_by(|left, right| {
        normalized_collision_key(&left.display_name)
            .cmp(&normalized_collision_key(&right.display_name))
            .then_with(|| left.id.cmp(&right.id))
    });

    let total = count(items.len());
    let start = usize::try_from(query.offset)
        .unwrap_or(usize::MAX)
        .min(items.len());
    let end = start
        .saturating_add(usize::from(query.limit))
        .min(items.len());
    let items = items.drain(start..end).collect();
    LibraryPage {
        items,
        total,
        offset: query.offset,
        limit: query.limit,
    }
}

fn managed_library_items(
    skills: Vec<crate::persistence::SkillRecord>,
    deployments: &[crate::persistence::DeploymentRecord],
) -> Vec<LibraryItem> {
    skills
        .into_iter()
        .filter(|skill| skill.lifecycle == crate::domain::SkillLifecycle::Active)
        .map(|skill| {
            let active_deployments = deployments
                .iter()
                .filter(|deployment| deployment.skill_id == skill.id && deployment.active)
                .count();
            let managed = active_deployments > 0;
            LibraryItem {
                id: format!("skill:{}", skill.id),
                skill_id: Some(skill.id.to_string()),
                display_name: skill.display_name,
                deployment_name: skill.deployment_name.to_string(),
                ownership: if managed {
                    LibraryOwnership::Managed
                } else {
                    LibraryOwnership::Vaulted
                },
                source_summary: if managed {
                    format!("Managed · {active_deployments} deployments")
                } else {
                    "Vaulted".to_owned()
                },
                locations: Vec::new(),
                digest: Some(skill.working_digest.to_string()),
                validation: LibraryValidation::Verified,
                duplicate_summary: DuplicateSummary {
                    exact_duplicate_locations: 0,
                    name_conflicts: 0,
                    probable_duplicates_or_renames: 0,
                    unverified: false,
                },
                deployment_count: count(active_deployments),
                working_location: Some(skill.working_path.to_string()),
                changed_at: skill.updated_at.to_string(),
                next_actions: Vec::new(),
            }
        })
        .collect()
}

fn library_item(
    mut group: ExternalGroup,
    name_digests: &BTreeMap<String, BTreeSet<BundleDigest>>,
    digest_names: &BTreeMap<BundleDigest, BTreeSet<String>>,
) -> LibraryItem {
    group.records.sort_by_key(|record| record.id);
    let first = &group.records[0];
    let changed_at = group
        .records
        .iter()
        .map(|record| record.observed_at)
        .max()
        .unwrap_or(first.observed_at);
    let verified = group
        .records
        .iter()
        .all(|record| record.status == "verified" && record.digest.is_some());
    let exact_duplicate_locations = group.records.len().saturating_sub(1);
    let name_conflicts = group.digest.map_or(0, |digest| {
        name_digests.get(&group.name_key).map_or(0, |digests| {
            digests.iter().filter(|other| **other != digest).count()
        })
    });
    let probable_duplicates = group.digest.map_or(0, |digest| {
        digest_names.get(&digest).map_or(0, |names| {
            names.iter().filter(|name| **name != group.name_key).count()
        })
    });
    let locations = group
        .records
        .iter()
        .map(|record| LibraryLocation {
            observation_id: record.id.to_string(),
            adapter_id: record.adapter_id.to_string(),
            source_root_id: record.source_root_id.clone(),
            path: record
                .display_path
                .to_str()
                .unwrap_or("Unsupported path")
                .to_owned(),
            status: record.status.clone(),
            error: record
                .error_code
                .as_ref()
                .zip(record.error_summary.as_ref())
                .map(|(code, summary)| LibraryErrorView {
                    code: code.clone(),
                    summary: summary.clone(),
                }),
        })
        .collect::<Vec<_>>();
    LibraryItem {
        id: format!("external:{}", first.id),
        skill_id: None,
        display_name: first.deployment_name.to_string(),
        deployment_name: first.deployment_name.to_string(),
        ownership: LibraryOwnership::External,
        source_summary: format!(
            "{} · {} {}",
            adapter_display_name(&first.adapter_id),
            locations.len(),
            if locations.len() == 1 {
                "location"
            } else {
                "locations"
            }
        ),
        locations,
        digest: group.digest.map(|digest| digest.to_string()),
        validation: if verified {
            LibraryValidation::Verified
        } else {
            LibraryValidation::Error
        },
        duplicate_summary: DuplicateSummary {
            exact_duplicate_locations: count(exact_duplicate_locations),
            name_conflicts: count(name_conflicts),
            probable_duplicates_or_renames: count(probable_duplicates),
            unverified: group.digest.is_none(),
        },
        deployment_count: 0,
        working_location: None,
        changed_at: changed_at.to_string(),
        next_actions: if verified {
            vec![
                LibraryAction::KeepExternal,
                LibraryAction::AddToVault,
                LibraryAction::AddAndManage,
            ]
        } else {
            vec![LibraryAction::KeepExternal]
        },
    }
}

fn adapter_display_name(adapter_id: &AdapterId) -> &'static str {
    if adapter_id.name() == "universal-agent-skills" {
        "Universal Agent Skills"
    } else {
        "Agent Skills"
    }
}

fn matches_search(item: &LibraryItem, search: Option<&str>) -> bool {
    let Some(search) = search else {
        return true;
    };
    normalized_collision_key(&item.display_name).contains(search)
        || item
            .working_location
            .as_deref()
            .is_some_and(|path| normalized_collision_key(path).contains(search))
        || item
            .locations
            .iter()
            .any(|location| normalized_collision_key(&location.path).contains(search))
}

fn matches_filter(item: &LibraryItem, filter: LibraryFilter) -> bool {
    match filter {
        LibraryFilter::All => true,
        LibraryFilter::Verified => item.validation == LibraryValidation::Verified,
        LibraryFilter::Errors => item.validation == LibraryValidation::Error,
        LibraryFilter::Conflicts => item.duplicate_summary.name_conflicts > 0,
        LibraryFilter::Duplicates => {
            item.duplicate_summary.exact_duplicate_locations > 0
                || item.duplicate_summary.probable_duplicates_or_renames > 0
        }
    }
}

#[derive(Debug, Error)]
pub enum ScanningServiceError {
    #[error("configured scan source is unknown")]
    UnknownSource,
    #[error("scan database failed: {0}")]
    Repository(RepositoryError),
    #[error("scan background worker failed: {0}")]
    Blocking(#[from] BlockingWorkError),
    #[error("scan job was not found")]
    JobNotFound,
    #[error("scan job state is unavailable")]
    StatePoisoned,
    #[error("configured path cannot be represented in the UI contract")]
    UnsupportedPath,
    #[error("Library page limit must be between 1 and {maximum}")]
    InvalidLibraryLimit { maximum: u16 },
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use crate::persistence::{DbExecutor, DbExecutorError, OpenVault};

    use super::*;

    fn time(value: i64) -> UtcTimestamp {
        UtcTimestamp::from_unix_millis(value).unwrap()
    }

    #[derive(Default)]
    struct CapturingScanEvents {
        progress: Mutex<Vec<ScanProgress>>,
        invalidations: Mutex<Vec<DomainInvalidated>>,
    }

    impl ScanEventSink for CapturingScanEvents {
        fn progress(&self, event: ScanProgress) {
            self.progress.lock().unwrap().push(event);
        }

        fn invalidated(&self, event: DomainInvalidated) {
            self.invalidations.lock().unwrap().push(event);
        }
    }

    fn job(id: ScanRunId, source_root_id: &str, state: ScanRunState) -> Arc<ScanJob> {
        Arc::new(ScanJob {
            cancellation: CancellationFlag::default(),
            view: Mutex::new(ScanRunView {
                job_id: id.to_string(),
                adapter_id: "universal-agent-skills@1".to_owned(),
                source_root_id: source_root_id.to_owned(),
                source_name: "Universal Agent Skills".to_owned(),
                display_root: "/skills".to_owned(),
                state,
                coverage: coverage_view(CoverageState::Partial),
                completed_entries: 0,
                estimated_entries: 0,
                observation_count: 0,
                error_count: 0,
                errors: Vec::new(),
                started_at: time(1_000).to_string(),
                completed_at: None,
            }),
        })
    }

    fn external_record(
        name: &str,
        digest: Option<BundleDigest>,
        path_suffix: &str,
    ) -> ExternalObservationRecord {
        ExternalObservationRecord {
            id: ObservationId::generate(),
            adapter_id: "universal-agent-skills@1".parse().unwrap(),
            source_root_kind: SOURCE_ROOT_KIND.to_owned(),
            source_root_id: "root".to_owned(),
            display_path: PathBuf::from(format!("/skills/{path_suffix}")),
            deployment_name: crate::domain::DeploymentName::parse(name).unwrap(),
            digest,
            status: if digest.is_some() {
                "verified".to_owned()
            } else {
                "hash_error".to_owned()
            },
            error_code: digest.is_none().then(|| "hash_io_failure".to_owned()),
            error_summary: digest.is_none().then(|| "Could not hash Bundle".to_owned()),
            first_seen_at: time(1_000),
            observed_at: time(2_000),
        }
    }

    #[test]
    fn library_grouping_is_digest_based_and_independent_of_input_order() {
        let one = BundleDigest::from_bytes([1; 32]);
        let two = BundleDigest::from_bytes([2; 32]);
        let records = vec![
            external_record("same", Some(one), "same-a"),
            external_record("same", Some(two), "same-conflict"),
            external_record("same", Some(one), "same-b"),
            external_record("rename", Some(one), "rename"),
            external_record("unknown", None, "unknown"),
        ];
        let mut reversed = records.clone();
        reversed.reverse();
        let query = || LibraryQuery {
            offset: 0,
            limit: 20,
            search: None,
            filter: LibraryFilter::All,
        };

        let first = build_library_page(records, Vec::new(), &[], &query());
        let second = build_library_page(reversed, Vec::new(), &[], &query());

        assert_eq!(first.total, 4);
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| (&item.display_name, item.locations.len()))
                .collect::<Vec<_>>(),
            second
                .items
                .iter()
                .map(|item| (&item.display_name, item.locations.len()))
                .collect::<Vec<_>>()
        );
        let exact = first
            .items
            .iter()
            .find(|item| item.display_name == "same" && item.locations.len() == 2)
            .unwrap();
        assert_eq!(exact.duplicate_summary.exact_duplicate_locations, 1);
        assert_eq!(exact.duplicate_summary.name_conflicts, 1);
        assert_eq!(exact.duplicate_summary.probable_duplicates_or_renames, 1);
    }

    #[test]
    fn library_query_filters_and_paginates_one_thousand_observations() {
        let records = (0..1_000)
            .map(|index| {
                external_record(
                    &format!("skill-{index:04}"),
                    Some(BundleDigest::from_bytes(ShaByte::from_index(index).0)),
                    &format!("skill-{index:04}"),
                )
            })
            .collect();

        let page = build_library_page(
            records,
            Vec::new(),
            &[],
            &LibraryQuery {
                offset: 500,
                limit: 25,
                search: Some("skill-".to_owned()),
                filter: LibraryFilter::Verified,
            },
        );

        assert_eq!(page.total, 1_000);
        assert_eq!(page.items.len(), 25);
        assert_eq!(page.items[0].display_name, "skill-0500");
    }

    struct ShaByte([u8; 32]);

    impl ShaByte {
        fn from_index(index: usize) -> Self {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&u64::try_from(index).unwrap().to_be_bytes());
            Self(bytes)
        }
    }

    #[test]
    fn duplicate_active_scan_for_one_root_collapses_to_the_existing_job() {
        let jobs = ScanJobs::default();
        let first_id = ScanRunId::generate();
        let first = job(first_id, "root", ScanRunState::Queued);
        assert!(
            jobs.insert_unless_active(first_id, Arc::clone(&first), "root")
                .unwrap()
                .is_none()
        );

        let duplicate_id = ScanRunId::generate();
        let existing = jobs
            .insert_unless_active(
                duplicate_id,
                job(duplicate_id, "root", ScanRunState::Queued),
                "root",
            )
            .unwrap()
            .unwrap();
        assert_eq!(existing.job_id, first_id.to_string());

        first.view.lock().unwrap().state = ScanRunState::Completed;
        let replacement_id = ScanRunId::generate();
        assert!(
            jobs.insert_unless_active(
                replacement_id,
                job(replacement_id, "root", ScanRunState::Queued),
                "root",
            )
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn service_scans_reconciles_and_cancels_without_touching_source_content() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let root = home.join(".agents/skills");
        fs::create_dir_all(&root).unwrap();
        for name in ["a", "b"] {
            let skill = root.join(name);
            fs::create_dir(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), name).unwrap();
        }
        let before = fs::read(root.join("a/SKILL.md")).unwrap();
        let vault = OpenVault::open(
            &directory.path().join("vault"),
            &directory.path().join("support"),
            std::slice::from_ref(&root),
        )
        .unwrap();
        let service = ScanningService::new(
            home,
            vault.repositories.clone(),
            BlockingWorkPool::new(2),
            ScanJobs::default(),
        );
        let events = Arc::new(CapturingScanEvents::default());
        let event_sink: Arc<dyn ScanEventSink> = events.clone();

        let job = service
            .start(
                ScanRequest {
                    source: ScanSource::UniversalGlobal,
                },
                event_sink,
            )
            .await
            .unwrap();
        let terminal = wait_for_terminal(&service, &job.job_id).await;

        assert_eq!(terminal.state, ScanRunState::Completed);
        assert_eq!(terminal.observation_count, 2);
        assert!(terminal.coverage.no_files_changed);
        assert_eq!(fs::read(root.join("a/SKILL.md")).unwrap(), before);
        assert_eq!(
            service
                .library_list(LibraryQuery {
                    offset: 0,
                    limit: 20,
                    search: None,
                    filter: LibraryFilter::All,
                })
                .await
                .unwrap()
                .total,
            2
        );
        {
            let invalidations = events.invalidations.lock().unwrap();
            assert_eq!(invalidations.len(), 1);
            assert_eq!(invalidations[0].revision, 1);
            assert_eq!(invalidations[0].scopes, ["scan", "library"]);
        }
        assert_eq!(
            events
                .progress
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.phase == "terminal")
                .count(),
            1
        );

        fs::remove_file(root.join("a/SKILL.md")).unwrap();
        fs::create_dir(root.join("a/SKILL.md")).unwrap();
        let event_sink: Arc<dyn ScanEventSink> = events.clone();
        let degraded = service
            .start(
                ScanRequest {
                    source: ScanSource::UniversalGlobal,
                },
                event_sink,
            )
            .await
            .unwrap();
        assert_eq!(
            wait_for_terminal(&service, &degraded.job_id).await.state,
            ScanRunState::CompletedWithErrors
        );
        let library = service
            .library_list(LibraryQuery {
                offset: 0,
                limit: 20,
                search: None,
                filter: LibraryFilter::All,
            })
            .await
            .unwrap();
        assert_eq!(library.total, 2);
        assert_eq!(
            library
                .items
                .iter()
                .find(|item| item.deployment_name == "a")
                .unwrap()
                .validation,
            LibraryValidation::Error
        );
        assert!(!service.cancel(&job.job_id).unwrap().accepted);
    }

    #[tokio::test]
    async fn failed_job_is_persisted_terminal_and_invalidates_scan_state() {
        let directory = tempfile::tempdir().unwrap();
        let database = DbExecutor::open(directory.path().join("index.sqlite")).unwrap();
        let service = ScanningService::new(
            directory.path().join("home"),
            Repositories::new(database.clone()),
            BlockingWorkPool::new(1),
            ScanJobs::default(),
        );
        let job_id = ScanRunId::generate();
        let job = job(job_id, "root", ScanRunState::Running);
        let events = Arc::new(CapturingScanEvents::default());
        let event_sink: Arc<dyn ScanEventSink> = events.clone();

        service
            .fail_job(
                job_id,
                time(1_000),
                "root".to_owned(),
                &job,
                &event_sink,
                "injected failure",
            )
            .await;

        assert_eq!(job.view.lock().unwrap().state, ScanRunState::Failed);
        let persisted_state = database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT state FROM scan_runs WHERE id = ?1",
                        [job_id.to_string()],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(DbExecutorError::Sqlite)
            })
            .unwrap();
        assert_eq!(persisted_state, "failed");
        let invalidations = events.invalidations.lock().unwrap();
        assert_eq!(invalidations.len(), 1);
        assert_eq!(invalidations[0].scopes, ["scan"]);
    }

    async fn wait_for_terminal(service: &ScanningService, id: &str) -> ScanRunView {
        for _ in 0..500 {
            let view = service.get(id).unwrap();
            if view.state.is_terminal() {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("scan did not finish");
    }

    #[test]
    fn repository_fixture_can_open_at_current_schema() {
        let directory = tempfile::tempdir().unwrap();
        let database = DbExecutor::open(directory.path().join("index.sqlite")).unwrap();
        assert_eq!(database.settings().unwrap().schema_version, 5);
    }

    #[test]
    fn configured_roots_persist_disable_override_custom_and_reenable_breadth() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let override_root = directory.path().join("cursor-override");
        let custom_root = directory.path().join("custom-global");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir(&override_root).unwrap();
        fs::create_dir(&custom_root).unwrap();
        let vault = OpenVault::open(
            &directory.path().join("vault"),
            &directory.path().join("support"),
            &[override_root.clone(), custom_root.clone()],
        )
        .unwrap();
        let now = UtcTimestamp::now();
        vault
            .repositories
            .upsert_adapter_configuration(crate::persistence::AdapterConfigurationRecord {
                adapter_name: "claude-code".into(),
                adapter_id: "claude-code@1".parse().unwrap(),
                enabled: false,
                global_override_path: None,
                project_override_path: None,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        vault
            .repositories
            .upsert_adapter_configuration(crate::persistence::AdapterConfigurationRecord {
                adapter_name: "cursor".into(),
                adapter_id: "cursor@1".parse().unwrap(),
                enabled: true,
                global_override_path: Some(override_root.clone()),
                project_override_path: Some(".custom/cursor-skills".into()),
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let custom_id = crate::domain::TargetId::generate();
        vault
            .repositories
            .upsert_target(crate::persistence::TargetRecord {
                id: custom_id,
                adapter_id: "custom-directory@1".parse().unwrap(),
                scope: "global".into(),
                root_path: custom_root.clone(),
                canonical_root_path: custom_root.canonicalize().unwrap(),
                project_id: None,
                is_override: false,
                is_custom: true,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let service = ScanningService::new(
            home,
            vault.repositories.clone(),
            BlockingWorkPool::new(2),
            ScanJobs::default(),
        );

        let roots = service.configured_global_roots().unwrap();
        assert_eq!(roots.len(), 7);
        assert!(
            roots
                .iter()
                .all(|root| root.adapter_id.name() != "claude-code")
        );
        assert!(
            roots
                .iter()
                .any(|root| root.source_root_id == "cursor@1:global-override")
        );
        assert!(roots.iter().any(|root| {
            root.source_root_id == format!("custom-directory@1:target:{custom_id}")
        }));

        vault
            .repositories
            .upsert_adapter_configuration(crate::persistence::AdapterConfigurationRecord {
                adapter_name: "claude-code".into(),
                adapter_id: "claude-code@1".parse().unwrap(),
                enabled: true,
                global_override_path: None,
                project_override_path: None,
                created_at: now,
                updated_at: UtcTimestamp::now(),
            })
            .unwrap();
        assert_eq!(service.configured_global_roots().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn six_enabled_default_roots_scan_independently_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        fs::create_dir(&home).unwrap();
        for root in global_roots(&home) {
            let skill = root.root.join("fixture-skill");
            fs::create_dir_all(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), root.adapter_id.to_string()).unwrap();
        }
        let vault = OpenVault::open(
            &directory.path().join("vault"),
            &directory.path().join("support"),
            &[],
        )
        .unwrap();
        let service = ScanningService::new(
            home,
            vault.repositories.clone(),
            BlockingWorkPool::new(2),
            ScanJobs::default(),
        );
        let events: Arc<dyn ScanEventSink> = Arc::new(CapturingScanEvents::default());
        let roots = service.configured_global_roots().unwrap();
        assert_eq!(roots.len(), 6);
        for root in roots {
            let manifest = root.root.join("fixture-skill/SKILL.md");
            let before = fs::read(&manifest).unwrap();
            let job = service
                .start(
                    ScanRequest {
                        source: ScanSource::ConfiguredGlobal(root.source_root_id),
                    },
                    Arc::clone(&events),
                )
                .await
                .unwrap();
            let terminal = wait_for_terminal(&service, &job.job_id).await;
            assert_eq!(terminal.state, ScanRunState::Completed);
            assert_eq!(terminal.observation_count, 1);
            assert_eq!(fs::read(manifest).unwrap(), before);
        }
    }
}
