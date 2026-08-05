//! Workspace Root authorization, project discovery, and durable coverage orchestration.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;
use thiserror::Error;

use crate::{
    adapters::DESCRIPTORS,
    domain::{ActivityId, ObservationId, ProjectId, ScanRunId, UtcTimestamp, WorkspaceRootId},
    filesystem::{BundleCaps, PathIdentity},
    persistence::{
        ActivityRecord, AuthorizationIdentityRecord, ObservationRecord, ProjectRecord,
        Repositories, RepositoryError, ScanErrorRecord, ScanReconciliation, ScanRunRecord,
        WorkspaceRootRecord,
    },
    scanner::{
        CancellationFlag, CoverageState, ManualProject, ProjectKind, ReconcileReason,
        ReconcileRequest, WatchCoordinator, WorkspaceAdapter, WorkspaceScanRequest, scan_workspace,
    },
};

const SOURCE_ROOT_KIND: &str = "workspace_root";
const PROJECT_SCOPE: &str = "project";

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRootAddRequest {
    pub selected_path: String,
    pub maximum_depth: Option<u8>,
    pub ignore_rules: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRootUpdateRequest {
    pub root_id: String,
    pub selected_path: Option<String>,
    pub maximum_depth: Option<u8>,
    pub ignore_rules: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRootPauseRequest {
    pub root_id: String,
    pub paused: bool,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRootIdRequest {
    pub root_id: String,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ManualProjectAddRequest {
    pub selected_path: String,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ManualProjectIdRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRootView {
    pub root_id: String,
    pub selected_path: String,
    pub canonical_path: String,
    pub enabled: bool,
    pub paused: bool,
    pub maximum_depth: u8,
    pub ignore_rules: Vec<String>,
    pub coverage_state: String,
    pub last_attempt: Option<String>,
    pub last_successful_complete_scan: Option<String>,
    pub project_count: u32,
    pub skill_count: u32,
    pub error_count: u32,
    pub errors: Vec<WorkspaceDiagnosticView>,
    pub no_files_changed: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDiagnosticView {
    pub path: String,
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ManualProjectView {
    pub project_id: String,
    pub root_path: String,
    pub canonical_path: String,
    pub git: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRemoveResult {
    pub root_id: String,
    pub removed: bool,
    pub no_files_changed: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceScanResultView {
    pub root_id: String,
    pub coverage_state: String,
    pub complete: bool,
    pub project_count: u32,
    pub skill_count: u32,
    pub error_count: u32,
    pub streamed_project_batches: u32,
    pub no_files_changed: bool,
}

#[derive(Debug, Clone, Serialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceProjectBatchEvent {
    pub root_id: String,
    pub project_root: String,
    pub project_kind: String,
    pub skill_count: u32,
    pub error_count: u32,
    pub observations: Vec<WorkspaceProjectObservationView>,
    pub diagnostics: Vec<WorkspaceDiagnosticView>,
    pub batch_complete: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceProjectObservationView {
    pub adapter_id: String,
    pub display_path: String,
    pub status: String,
}

pub trait WorkspaceEventSink: Send + Sync + 'static {
    fn project_batch(&self, event: WorkspaceProjectBatchEvent);
}

#[derive(Debug)]
struct NoWorkspaceEvents;

impl WorkspaceEventSink for NoWorkspaceEvents {
    fn project_batch(&self, _event: WorkspaceProjectBatchEvent) {}
}

#[derive(Clone)]
pub struct WorkspaceService {
    repositories: Repositories,
    vault_root: PathBuf,
    watcher: Arc<Mutex<WatchCoordinator>>,
    gate: Arc<Mutex<()>>,
}

impl WorkspaceService {
    #[must_use]
    pub fn new(
        repositories: Repositories,
        vault_root: PathBuf,
        watcher: Arc<Mutex<WatchCoordinator>>,
        gate: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            repositories,
            vault_root,
            watcher,
            gate,
        }
    }

    pub fn initialize_reconciliation(&self) -> Result<(), WorkspaceError> {
        self.refresh_watcher_boundaries()?;
        let mut watcher = self
            .watcher
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        watcher.proactive(ReconcileReason::Startup);
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn add(
        &self,
        request: WorkspaceRootAddRequest,
    ) -> Result<WorkspaceRootView, WorkspaceError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        let depth = validate_depth(
            request
                .maximum_depth
                .unwrap_or(WorkspaceScanRequest::DEFAULT_MAX_DEPTH),
        )?;
        let (selected_path, canonical_path, identity) =
            self.authorize_workspace_path(&request.selected_path, None)?;
        let now = UtcTimestamp::now();
        let root = WorkspaceRootRecord {
            id: WorkspaceRootId::generate(),
            selected_path,
            canonical_path: canonical_path.clone(),
            paused: false,
            maximum_depth: usize::from(depth),
            ignore_rules: serde_json::json!(request.ignore_rules),
            scan_status: "never_scanned".to_owned(),
            created_at: now,
            updated_at: now,
        };
        self.repositories
            .upsert_workspace_root_authorization(root.clone(), identity)?;
        self.refresh_watcher_boundaries()?;
        self.view(root)
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn update(
        &self,
        request: WorkspaceRootUpdateRequest,
    ) -> Result<WorkspaceRootView, WorkspaceError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        let id = parse_root_id(&request.root_id)?;
        let mut root = self.root(id)?;
        let previous = root.canonical_path.clone();
        let mut replacement_identity = None;
        if let Some(path) = request.selected_path {
            let (selected, canonical, identity) = self.authorize_workspace_path(&path, Some(id))?;
            root.selected_path = selected;
            root.canonical_path = canonical;
            "never_scanned".clone_into(&mut root.scan_status);
            replacement_identity = Some(identity);
        }
        if let Some(depth) = request.maximum_depth {
            root.maximum_depth = usize::from(validate_depth(depth)?);
        }
        if let Some(rules) = request.ignore_rules {
            root.ignore_rules = serde_json::json!(rules);
        }
        root.updated_at = UtcTimestamp::now();
        if let Some(identity) = replacement_identity {
            self.repositories
                .upsert_workspace_root_authorization(root.clone(), identity)?;
        } else {
            self.repositories.upsert_workspace_root(root.clone())?;
        }
        if previous != root.canonical_path {
            self.repositories
                .rehome_workspace_manual_observations(root.id)?;
        }
        self.refresh_watcher_boundaries()?;
        if previous != root.canonical_path {
            self.watcher
                .lock()
                .map_err(|_| WorkspaceError::StatePoisoned)?
                .proactive(ReconcileReason::RootReplaced);
        }
        self.view(root)
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn pause(
        &self,
        request: WorkspaceRootPauseRequest,
    ) -> Result<WorkspaceRootView, WorkspaceError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        let id = parse_root_id(&request.root_id)?;
        let mut root = self.root(id)?;
        root.paused = request.paused;
        root.updated_at = UtcTimestamp::now();
        self.repositories.upsert_workspace_root(root.clone())?;
        if root.paused {
            self.repositories
                .rehome_workspace_manual_observations(root.id)?;
        }
        self.refresh_watcher_boundaries()?;
        if !root.paused {
            self.watcher
                .lock()
                .map_err(|_| WorkspaceError::StatePoisoned)?
                .proactive(ReconcileReason::Resume);
        }
        self.view(root)
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn remove(
        &self,
        request: WorkspaceRootIdRequest,
    ) -> Result<WorkspaceRemoveResult, WorkspaceError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        let id = parse_root_id(&request.root_id)?;
        let removed = self.repositories.remove_workspace_root(id)?;
        self.refresh_watcher_boundaries()?;
        if removed {
            self.watcher
                .lock()
                .map_err(|_| WorkspaceError::StatePoisoned)?
                .proactive(ReconcileReason::RootReplaced);
        }
        Ok(WorkspaceRemoveResult {
            root_id: id.to_string(),
            removed,
            no_files_changed: true,
        })
    }

    pub fn list(&self) -> Result<Vec<WorkspaceRootView>, WorkspaceError> {
        self.repositories
            .workspace_roots()?
            .into_iter()
            .map(|root| self.view(root))
            .collect()
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn add_manual_project(
        &self,
        request: ManualProjectAddRequest,
    ) -> Result<ManualProjectView, WorkspaceError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        let (root_path, canonical_path, identity) =
            self.inspect_authorized_path(&request.selected_path)?;
        let git = is_git_project(&canonical_path);
        let existing = self
            .repositories
            .project_by_canonical_path(&canonical_path)?;
        let now = UtcTimestamp::now();
        let project = ProjectRecord {
            id: existing
                .as_ref()
                .map_or_else(ProjectId::generate, |item| item.id),
            workspace_root_id: None,
            root_path,
            canonical_path,
            discovery_evidence: "manual_selection".to_owned(),
            git_classification: if git { "git" } else { "non_git" }.to_owned(),
            manual: true,
            created_at: existing.as_ref().map_or(now, |item| item.created_at),
            updated_at: now,
        };
        self.repositories
            .upsert_manual_project_authorization(project.clone(), identity)?;
        self.refresh_watcher_boundaries()?;
        self.watcher
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?
            .proactive(ReconcileReason::Startup);
        Ok(manual_project_view(&project))
    }

    pub fn manual_projects(&self) -> Result<Vec<ManualProjectView>, WorkspaceError> {
        Ok(self
            .repositories
            .manual_projects()?
            .into_iter()
            .map(|project| manual_project_view(&project))
            .collect())
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn rescan_manual_project(
        &self,
        request: ManualProjectIdRequest,
    ) -> Result<WorkspaceScanResultView, WorkspaceError> {
        let project_id = request
            .project_id
            .parse::<ProjectId>()
            .map_err(|_| WorkspaceError::InvalidProjectId)?;
        let project = self
            .repositories
            .project(project_id)?
            .filter(|project| project.manual)
            .ok_or(WorkspaceError::ProjectMissing)?;
        if let Some(owner) = self
            .repositories
            .workspace_roots()?
            .into_iter()
            .find(|root| !root.paused && project.canonical_path.starts_with(&root.canonical_path))
        {
            return self.rescan(WorkspaceRootIdRequest {
                root_id: owner.id.to_string(),
            });
        }
        let _guard = self
            .gate
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        let identity = self
            .repositories
            .manual_project_identity(project_id)?
            .ok_or(WorkspaceError::MissingIdentity)?;
        let adapters = DESCRIPTORS
            .into_iter()
            .map(|adapter| WorkspaceAdapter {
                adapter_id: adapter.id(),
                target_suffix: PathBuf::from(adapter.project_path),
            })
            .collect::<Vec<_>>();
        let started_at = UtcTimestamp::now();
        let mut streamed_batches = 0_u32;
        let result = scan_workspace(
            &WorkspaceScanRequest {
                source_root_id: project_id.to_string(),
                selected_root: project.root_path.clone(),
                canonical_root: project.canonical_path.clone(),
                device_id: identity.device_id,
                file_id: identity.file_id,
                max_depth: 1,
                user_ignores: Vec::new(),
                adapters: adapters.clone(),
                manual_projects: vec![ManualProject {
                    root: project.root_path.clone(),
                    is_git: project.git_classification == "git",
                    device_id: identity.device_id,
                    file_id: identity.file_id,
                }],
                caps: BundleCaps::default(),
                cancellation: CancellationFlag::default(),
            },
            |_| streamed_batches = streamed_batches.saturating_add(1),
        );
        let completed_at = UtcTimestamp::now();
        let complete = result.coverage.is_complete();
        for adapter in adapters {
            let run_id = ScanRunId::generate();
            let mut diagnostics = result.diagnostics.clone();
            let mut observations = Vec::new();
            for batch in &result.batches {
                diagnostics.extend(batch.diagnostics.clone());
                observations.extend(
                    batch
                        .observations
                        .iter()
                        .filter(|observation| observation.adapter_id == adapter.adapter_id)
                        .map(|observation| ObservationRecord {
                            id: ObservationId::generate(),
                            skill_id: observation.skill_id,
                            adapter_id: observation.adapter_id.clone(),
                            scope: PROJECT_SCOPE.to_owned(),
                            project_id: Some(project_id),
                            source_root_kind: "manual_project".to_owned(),
                            source_root_id: project_id.to_string(),
                            display_path: observation.display_path.clone(),
                            normalized_path: observation.normalized_path.clone(),
                            canonical_path: observation.canonical_path.clone(),
                            deployment_name: observation.deployment_name.clone(),
                            digest: observation.digest,
                            status: observation.status.as_str().to_owned(),
                            error_code: observation.error.as_ref().map(|item| item.code.to_owned()),
                            error_summary: observation
                                .error
                                .as_ref()
                                .map(|item| item.summary.clone()),
                            last_successful_run_id: complete.then_some(run_id),
                            first_seen_at: completed_at,
                            observed_at: completed_at,
                            stale_at: None,
                        }),
                );
            }
            self.repositories.reconcile_scan(ScanReconciliation {
                run: ScanRunRecord {
                    id: run_id,
                    root_kind: "manual_project".to_owned(),
                    root_id: Some(project_id.to_string()),
                    scope: PROJECT_SCOPE.to_owned(),
                    state: scan_state(result.coverage).to_owned(),
                    coverage: serde_json::json!({
                        "state": coverage_text(result.coverage),
                        "complete": complete,
                        "observationCount": observations.len(),
                        "errorCount": diagnostics.len(),
                        "noFilesChanged": true
                    }),
                    started_at,
                    completed_at: Some(completed_at),
                },
                adapter_id: adapter.adapter_id.clone(),
                scope: PROJECT_SCOPE.to_owned(),
                source_root_kind: "manual_project".to_owned(),
                source_root_id: project_id.to_string(),
                observations,
                errors: diagnostics
                    .iter()
                    .map(|diagnostic| ScanErrorRecord {
                        scan_run_id: run_id,
                        path: diagnostic.path.clone(),
                        error_code: diagnostic.code.to_owned(),
                        summary: diagnostic.summary.clone(),
                    })
                    .collect(),
                coverage_complete: complete,
                activity: ActivityRecord {
                    id: ActivityId::generate(),
                    operation_id: None,
                    kind: "manual_project_scan".to_owned(),
                    state: scan_state(result.coverage).to_owned(),
                    outcome: None,
                    summary: format!(
                        "Manual project scan finished with {} diagnostic(s)",
                        diagnostics.len()
                    ),
                    details: serde_json::json!({
                        "projectId": project_id,
                        "adapterId": adapter.adapter_id,
                        "coverage": coverage_text(result.coverage),
                        "noFilesChanged": true
                    }),
                    started_at,
                    completed_at: Some(completed_at),
                },
            })?;
        }
        let skill_count = result
            .batches
            .iter()
            .map(|batch| batch.observations.len())
            .sum();
        let error_count = result.diagnostics.len()
            + result
                .batches
                .iter()
                .map(|batch| batch.diagnostics.len())
                .sum::<usize>();
        Ok(WorkspaceScanResultView {
            root_id: project_id.to_string(),
            coverage_state: coverage_text(result.coverage).to_owned(),
            complete,
            project_count: count(result.batches.len()),
            skill_count: count(skill_count),
            error_count: count(error_count),
            streamed_project_batches: streamed_batches,
            no_files_changed: true,
        })
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn reconcile_request(
        &self,
        request: ReconcileRequest,
    ) -> Vec<Result<WorkspaceScanResultView, WorkspaceError>> {
        let roots = match self.repositories.workspace_roots() {
            Ok(roots) => roots,
            Err(error) => return vec![Err(error.into())],
        };
        let manuals = match self.repositories.manual_projects() {
            Ok(projects) => projects,
            Err(error) => return vec![Err(error.into())],
        };
        let active_roots = roots
            .iter()
            .filter(|root| !root.paused)
            .cloned()
            .collect::<Vec<_>>();
        let selected_roots = match &request {
            ReconcileRequest::BoundedFull { .. } => active_roots.clone(),
            ReconcileRequest::Targeted(invalidations) => active_roots
                .iter()
                .filter(|root| {
                    invalidations
                        .iter()
                        .any(|item| item.path == root.canonical_path)
                })
                .cloned()
                .collect(),
        };
        let standalone_manuals = manuals.into_iter().filter(|project| {
            !active_roots
                .iter()
                .any(|root| project.canonical_path.starts_with(&root.canonical_path))
        });
        let selected_manuals = match &request {
            ReconcileRequest::BoundedFull { .. } => standalone_manuals.collect::<Vec<_>>(),
            ReconcileRequest::Targeted(invalidations) => standalone_manuals
                .into_iter()
                .filter(|project| {
                    invalidations
                        .iter()
                        .any(|item| item.path == project.canonical_path)
                })
                .collect(),
        };
        let mut results = selected_roots
            .into_iter()
            .map(|root| {
                self.rescan(WorkspaceRootIdRequest {
                    root_id: root.id.to_string(),
                })
            })
            .collect::<Vec<_>>();
        results.extend(selected_manuals.into_iter().map(|project| {
            self.rescan_manual_project(ManualProjectIdRequest {
                project_id: project.id.to_string(),
            })
        }));
        results
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn rescan(
        &self,
        request: WorkspaceRootIdRequest,
    ) -> Result<WorkspaceScanResultView, WorkspaceError> {
        self.rescan_with_events(request, Arc::new(NoWorkspaceEvents))
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::needless_pass_by_value)]
    pub fn rescan_with_events(
        &self,
        request: WorkspaceRootIdRequest,
        events: Arc<dyn WorkspaceEventSink>,
    ) -> Result<WorkspaceScanResultView, WorkspaceError> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?;
        let id = parse_root_id(&request.root_id)?;
        let mut root = self.root(id)?;
        if root.paused {
            return Err(WorkspaceError::RootPaused);
        }
        "scanning".clone_into(&mut root.scan_status);
        root.updated_at = UtcTimestamp::now();
        self.repositories.upsert_workspace_root(root.clone())?;
        let ignores = serde_json::from_value::<Vec<String>>(root.ignore_rules.clone())
            .map_err(WorkspaceError::InvalidIgnoreProjection)?;
        let identity = self
            .repositories
            .workspace_root_identity(id)?
            .ok_or(WorkspaceError::MissingIdentity)?;
        let adapters = DESCRIPTORS
            .into_iter()
            .map(|adapter| WorkspaceAdapter {
                adapter_id: adapter.id(),
                target_suffix: PathBuf::from(adapter.project_path),
            })
            .collect::<Vec<_>>();
        let manual_projects = self
            .repositories
            .manual_projects()?
            .into_iter()
            .filter(|project| project.canonical_path.starts_with(&root.canonical_path))
            .map(|project| {
                let identity = self
                    .repositories
                    .manual_project_identity(project.id)?
                    .ok_or(WorkspaceError::MissingIdentity)?;
                Ok(ManualProject {
                    root: project.root_path,
                    is_git: project.git_classification == "git",
                    device_id: identity.device_id,
                    file_id: identity.file_id,
                })
            })
            .collect::<Result<Vec<_>, WorkspaceError>>()?;
        let started_at = UtcTimestamp::now();
        let mut streamed_batches = 0_u32;
        let event_root_id = id.to_string();
        let result = scan_workspace(
            &WorkspaceScanRequest {
                source_root_id: id.to_string(),
                selected_root: root.selected_path.clone(),
                canonical_root: root.canonical_path.clone(),
                device_id: identity.device_id,
                file_id: identity.file_id,
                max_depth: u8::try_from(root.maximum_depth)
                    .map_err(|_| WorkspaceError::InvalidDepth)?,
                user_ignores: ignores,
                adapters: adapters.clone(),
                manual_projects,
                caps: BundleCaps::default(),
                cancellation: CancellationFlag::default(),
            },
            |batch| {
                streamed_batches = streamed_batches.saturating_add(1);
                events.project_batch(WorkspaceProjectBatchEvent {
                    root_id: event_root_id.clone(),
                    project_root: batch.project_root.to_string_lossy().into_owned(),
                    project_kind: project_evidence(batch.kind).to_owned(),
                    skill_count: count(batch.observations.len()),
                    error_count: count(batch.diagnostics.len()),
                    observations: batch
                        .observations
                        .iter()
                        .map(|observation| WorkspaceProjectObservationView {
                            adapter_id: observation.adapter_id.to_string(),
                            display_path: observation.display_path.to_string_lossy().into_owned(),
                            status: observation.status.as_str().to_owned(),
                        })
                        .collect(),
                    diagnostics: batch
                        .diagnostics
                        .iter()
                        .map(|diagnostic| WorkspaceDiagnosticView {
                            path: diagnostic.path.to_string_lossy().into_owned(),
                            code: diagnostic.code.to_owned(),
                            summary: diagnostic.summary.clone(),
                        })
                        .collect(),
                    batch_complete: batch.batch_complete,
                });
            },
        );
        let completed_at = UtcTimestamp::now();
        let mut projects = BTreeMap::new();
        for batch in &result.batches {
            let canonical = batch
                .project_root
                .canonicalize()
                .unwrap_or_else(|_| batch.project_root.clone());
            let existing = self.repositories.project_by_canonical_path(&canonical)?;
            let now = UtcTimestamp::now();
            let manual = matches!(
                batch.kind,
                ProjectKind::ManualGit | ProjectKind::ManualNonGit
            );
            let project = ProjectRecord {
                id: existing
                    .as_ref()
                    .map_or_else(ProjectId::generate, |item| item.id),
                workspace_root_id: (!manual).then_some(id),
                root_path: batch.project_root.clone(),
                canonical_path: canonical.clone(),
                discovery_evidence: project_evidence(batch.kind).to_owned(),
                git_classification: if matches!(
                    batch.kind,
                    ProjectKind::Git | ProjectKind::ManualGit
                ) {
                    "git"
                } else {
                    "non_git"
                }
                .to_owned(),
                manual,
                created_at: existing.as_ref().map_or(now, |item| item.created_at),
                updated_at: now,
            };
            self.repositories.upsert_project(project.clone())?;
            projects.insert(canonical, project.id);
        }
        let complete = result.coverage.is_complete();
        for adapter in adapters {
            let adapter_id = adapter.adapter_id;
            let run_id = ScanRunId::generate();
            let mut observations = Vec::new();
            let mut diagnostics = result.diagnostics.clone();
            for batch in &result.batches {
                diagnostics.extend(batch.diagnostics.clone());
                let canonical = batch
                    .project_root
                    .canonicalize()
                    .unwrap_or_else(|_| batch.project_root.clone());
                let project_id = projects.get(&canonical).copied();
                observations.extend(
                    batch
                        .observations
                        .iter()
                        .filter(|observation| observation.adapter_id == adapter_id)
                        .map(|observation| ObservationRecord {
                            id: ObservationId::generate(),
                            skill_id: observation.skill_id,
                            adapter_id: observation.adapter_id.clone(),
                            scope: PROJECT_SCOPE.to_owned(),
                            project_id,
                            source_root_kind: SOURCE_ROOT_KIND.to_owned(),
                            source_root_id: id.to_string(),
                            display_path: observation.display_path.clone(),
                            normalized_path: observation.normalized_path.clone(),
                            canonical_path: observation.canonical_path.clone(),
                            deployment_name: observation.deployment_name.clone(),
                            digest: observation.digest,
                            status: observation.status.as_str().to_owned(),
                            error_code: observation.error.as_ref().map(|item| item.code.to_owned()),
                            error_summary: observation
                                .error
                                .as_ref()
                                .map(|item| item.summary.clone()),
                            last_successful_run_id: complete.then_some(run_id),
                            first_seen_at: completed_at,
                            observed_at: completed_at,
                            stale_at: None,
                        }),
                );
            }
            self.repositories.reconcile_scan(ScanReconciliation {
                run: workspace_scan_run(
                    run_id,
                    id,
                    result.coverage,
                    observations.len(),
                    diagnostics.len(),
                    started_at,
                    completed_at,
                ),
                adapter_id: adapter_id.clone(),
                scope: PROJECT_SCOPE.to_owned(),
                source_root_kind: SOURCE_ROOT_KIND.to_owned(),
                source_root_id: id.to_string(),
                observations,
                errors: diagnostics
                    .iter()
                    .map(|diagnostic| ScanErrorRecord {
                        scan_run_id: run_id,
                        path: diagnostic.path.clone(),
                        error_code: diagnostic.code.to_owned(),
                        summary: diagnostic.summary.clone(),
                    })
                    .collect(),
                coverage_complete: complete,
                activity: ActivityRecord {
                    id: ActivityId::generate(),
                    operation_id: None,
                    kind: "workspace_scan".to_owned(),
                    state: scan_state(result.coverage).to_owned(),
                    outcome: None,
                    summary: format!(
                        "Workspace scan finished with {} diagnostic(s)",
                        diagnostics.len()
                    ),
                    details: serde_json::json!({
                        "workspaceRootId": id,
                        "adapterId": adapter_id,
                        "coverage": coverage_text(result.coverage),
                        "noFilesChanged": true
                    }),
                    started_at,
                    completed_at: Some(completed_at),
                },
            })?;
        }
        coverage_text(result.coverage).clone_into(&mut root.scan_status);
        root.updated_at = completed_at;
        self.repositories.upsert_workspace_root(root)?;
        let skill_count = result
            .batches
            .iter()
            .map(|batch| batch.observations.len())
            .sum::<usize>();
        let error_count = result.diagnostics.len()
            + result
                .batches
                .iter()
                .map(|batch| batch.diagnostics.len())
                .sum::<usize>();
        Ok(WorkspaceScanResultView {
            root_id: result.source_root_id,
            coverage_state: coverage_text(result.coverage).to_owned(),
            complete,
            project_count: count(result.batches.len()),
            skill_count: count(skill_count),
            error_count: count(error_count),
            streamed_project_batches: streamed_batches,
            no_files_changed: true,
        })
    }

    fn root(&self, id: WorkspaceRootId) -> Result<WorkspaceRootRecord, WorkspaceError> {
        self.repositories
            .workspace_root(id)?
            .ok_or(WorkspaceError::RootMissing)
    }

    fn refresh_watcher_boundaries(&self) -> Result<(), WorkspaceError> {
        let roots = self
            .repositories
            .workspace_roots()?
            .into_iter()
            .filter(|root| !root.paused)
            .collect::<Vec<_>>();
        let mut boundaries = roots
            .iter()
            .map(|root| root.canonical_path.clone())
            .collect::<Vec<_>>();
        boundaries.extend(
            self.repositories
                .manual_projects()?
                .into_iter()
                .filter(|project| {
                    !roots
                        .iter()
                        .any(|root| project.canonical_path.starts_with(&root.canonical_path))
                })
                .map(|project| project.canonical_path),
        );
        self.watcher
            .lock()
            .map_err(|_| WorkspaceError::StatePoisoned)?
            .replace_boundaries(boundaries);
        Ok(())
    }

    fn authorize_workspace_path(
        &self,
        selected: &str,
        replacing: Option<WorkspaceRootId>,
    ) -> Result<(PathBuf, PathBuf, AuthorizationIdentityRecord), WorkspaceError> {
        let authorized = self.inspect_authorized_path(selected)?;
        for root in self.repositories.workspace_roots()? {
            if Some(root.id) != replacing
                && (authorized.1.starts_with(&root.canonical_path)
                    || root.canonical_path.starts_with(&authorized.1))
            {
                return Err(WorkspaceError::OverlappingRoot);
            }
        }
        Ok(authorized)
    }

    fn inspect_authorized_path(
        &self,
        selected: &str,
    ) -> Result<(PathBuf, PathBuf, AuthorizationIdentityRecord), WorkspaceError> {
        let selected = PathBuf::from(selected);
        if !selected.is_absolute() {
            return Err(WorkspaceError::PathNotAbsolute);
        }
        let metadata = fs::symlink_metadata(&selected).map_err(WorkspaceError::ReadPath)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorkspaceError::PathNotDirectory);
        }
        let canonical = selected.canonicalize().map_err(WorkspaceError::ReadPath)?;
        let vault = self
            .vault_root
            .canonicalize()
            .unwrap_or_else(|_| self.vault_root.clone());
        if canonical.starts_with(&vault) || vault.starts_with(&canonical) {
            return Err(WorkspaceError::VaultOverlap);
        }
        let identity = PathIdentity::from_metadata(&metadata);
        Ok((
            selected,
            canonical,
            AuthorizationIdentityRecord {
                device_id: identity.device_id,
                file_id: identity.file_id,
            },
        ))
    }

    fn view(&self, root: WorkspaceRootRecord) -> Result<WorkspaceRootView, WorkspaceError> {
        let coverage = self.repositories.workspace_coverage(root.id)?;
        let project_count = self
            .repositories
            .workspace_observed_project_count(root.id)?;
        let skill_count = self.repositories.workspace_observation_count(root.id)?;
        let expected_identity = self.repositories.workspace_root_identity(root.id)?;
        let actual_identity = fs::symlink_metadata(&root.selected_path)
            .ok()
            .map(|metadata| PathIdentity::from_metadata(&metadata));
        let identity_matches = root.selected_path.canonicalize().ok().as_ref()
            == Some(&root.canonical_path)
            && expected_identity.is_some_and(|expected| {
                actual_identity.is_some_and(|actual| {
                    expected.device_id == actual.device_id && expected.file_id == actual.file_id
                })
            });
        let state = if !identity_matches {
            "stale"
        } else if coverage.latest.is_none() {
            "never_scanned"
        } else {
            root.scan_status.as_str()
        };
        Ok(WorkspaceRootView {
            root_id: root.id.to_string(),
            selected_path: path_display(&root.selected_path)?,
            canonical_path: path_display(&root.canonical_path)?,
            enabled: !root.paused,
            paused: root.paused,
            maximum_depth: u8::try_from(root.maximum_depth)
                .map_err(|_| WorkspaceError::InvalidDepth)?,
            ignore_rules: serde_json::from_value(root.ignore_rules)
                .map_err(WorkspaceError::InvalidIgnoreProjection)?,
            coverage_state: state.to_owned(),
            last_attempt: coverage.latest.map(|run| run.started_at.to_string()),
            last_successful_complete_scan: coverage
                .last_successful_complete
                .map(|time| time.to_string()),
            project_count,
            skill_count,
            error_count: coverage.total_errors,
            errors: coverage
                .errors
                .into_iter()
                .map(|error| WorkspaceDiagnosticView {
                    path: error.path.to_string_lossy().into_owned(),
                    code: error.error_code,
                    summary: error.summary,
                })
                .collect(),
            no_files_changed: true,
        })
    }
}

fn workspace_scan_run(
    id: ScanRunId,
    root_id: WorkspaceRootId,
    coverage: CoverageState,
    observations: usize,
    errors: usize,
    started_at: UtcTimestamp,
    completed_at: UtcTimestamp,
) -> ScanRunRecord {
    ScanRunRecord {
        id,
        root_kind: SOURCE_ROOT_KIND.to_owned(),
        root_id: Some(root_id.to_string()),
        scope: PROJECT_SCOPE.to_owned(),
        state: scan_state(coverage).to_owned(),
        coverage: serde_json::json!({
            "state": coverage_text(coverage),
            "complete": coverage.is_complete(),
            "observationCount": observations,
            "errorCount": errors,
            "noFilesChanged": true
        }),
        started_at,
        completed_at: Some(completed_at),
    }
}

fn validate_depth(depth: u8) -> Result<u8, WorkspaceError> {
    (1..=32)
        .contains(&depth)
        .then_some(depth)
        .ok_or(WorkspaceError::InvalidDepth)
}

fn parse_root_id(value: &str) -> Result<WorkspaceRootId, WorkspaceError> {
    value.parse().map_err(|_| WorkspaceError::InvalidRootId)
}

fn is_git_project(path: &Path) -> bool {
    fs::symlink_metadata(path.join(".git"))
        .is_ok_and(|metadata| metadata.is_dir() || metadata.is_file())
}

fn manual_project_view(project: &ProjectRecord) -> ManualProjectView {
    ManualProjectView {
        project_id: project.id.to_string(),
        root_path: project.root_path.to_string_lossy().into_owned(),
        canonical_path: project.canonical_path.to_string_lossy().into_owned(),
        git: project.git_classification == "git",
    }
}

fn project_evidence(kind: ProjectKind) -> &'static str {
    match kind {
        ProjectKind::Git => "git_boundary",
        ProjectKind::Implicit => "adapter_target_suffix",
        ProjectKind::ManualGit | ProjectKind::ManualNonGit => "manual_selection",
    }
}

fn coverage_text(coverage: CoverageState) -> &'static str {
    match coverage {
        CoverageState::Complete => "complete",
        CoverageState::Missing => "missing",
        CoverageState::Inaccessible => "inaccessible",
        CoverageState::InvalidRoot => "invalid_root",
        CoverageState::Partial => "incomplete",
        CoverageState::Cancelled => "cancelled",
    }
}

fn scan_state(coverage: CoverageState) -> &'static str {
    match coverage {
        CoverageState::Complete => "completed",
        CoverageState::Cancelled => "cancelled",
        CoverageState::Missing
        | CoverageState::Inaccessible
        | CoverageState::InvalidRoot
        | CoverageState::Partial => "completed_with_errors",
    }
}

fn path_display(path: &Path) -> Result<String, WorkspaceError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(WorkspaceError::UnsupportedPath)
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("Workspace Root ID is invalid")]
    InvalidRootId,
    #[error("Manual project ID is invalid")]
    InvalidProjectId,
    #[error("Manual project was not found")]
    ProjectMissing,
    #[error("Workspace Root was not found")]
    RootMissing,
    #[error("Workspace Root is paused")]
    RootPaused,
    #[error("Workspace depth must be between 1 and 32")]
    InvalidDepth,
    #[error("Workspace path must be absolute")]
    PathNotAbsolute,
    #[error("Workspace path must be a real directory, not a file or symlink")]
    PathNotDirectory,
    #[error("Workspace Root cannot overlap the Vault")]
    VaultOverlap,
    #[error("Workspace Roots cannot overlap in M0")]
    OverlappingRoot,
    #[error("Workspace authorization identity is missing")]
    MissingIdentity,
    #[error("Workspace path cannot be read: {0}")]
    ReadPath(std::io::Error),
    #[error("Workspace path cannot be represented safely")]
    UnsupportedPath,
    #[error("Workspace ignore settings are invalid: {0}")]
    InvalidIgnoreProjection(serde_json::Error),
    #[error("Workspace database failed: {0}")]
    Repository(#[from] RepositoryError),
    #[error("Workspace reconciliation state is unavailable")]
    StatePoisoned,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::persistence::OpenVault;

    use super::*;

    #[test]
    fn depth_contract_is_bounded() {
        assert_eq!(validate_depth(1).unwrap(), 1);
        assert_eq!(validate_depth(32).unwrap(), 32);
        assert!(validate_depth(0).is_err());
        assert!(validate_depth(33).is_err());
    }

    #[test]
    fn pausing_root_rehomes_contained_manual_evidence_before_incomplete_rescan() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let manual_project = workspace.join("manual");
        let skill = manual_project.join(".agents/skills/example");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: Example\n---\n").unwrap();
        let vault = Arc::new(
            OpenVault::open(
                &directory.path().join("vault"),
                &directory.path().join("support"),
                &[],
            )
            .unwrap(),
        );
        let service = WorkspaceService::new(
            vault.repositories.clone(),
            vault.paths.root().to_path_buf(),
            Arc::new(Mutex::new(WatchCoordinator::default())),
            Arc::new(Mutex::new(())),
        );
        let root = service
            .add(WorkspaceRootAddRequest {
                selected_path: workspace.to_string_lossy().into_owned(),
                maximum_depth: None,
                ignore_rules: Vec::new(),
            })
            .unwrap();
        let manual = service
            .add_manual_project(ManualProjectAddRequest {
                selected_path: manual_project.to_string_lossy().into_owned(),
            })
            .unwrap();
        assert!(
            service
                .rescan(WorkspaceRootIdRequest {
                    root_id: root.root_id.clone(),
                })
                .unwrap()
                .complete
        );

        service
            .pause(WorkspaceRootPauseRequest {
                root_id: root.root_id,
                paused: true,
            })
            .unwrap();
        fs::rename(&manual_project, workspace.join("manual-unavailable")).unwrap();
        let incomplete = service
            .rescan_manual_project(ManualProjectIdRequest {
                project_id: manual.project_id.clone(),
            })
            .unwrap();
        assert!(!incomplete.complete);
        let project_id = manual.project_id;
        let active = vault
            .database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM observations
                         WHERE project_id = ?1 AND source_root_kind = 'manual_project'
                           AND source_root_id = ?1 AND stale_at_ms IS NULL",
                        [project_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)
            })
            .unwrap();
        assert!(active > 0);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn root_lifecycle_and_rescan_are_read_only_and_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let skill = workspace.join(".agents/skills/example");
        fs::create_dir_all(&skill).unwrap();
        fs::create_dir(workspace.join(".git")).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: Example\n---\n").unwrap();
        let manual_project = directory.path().join("manual-project");
        let manual_skill = manual_project.join(".agents/skills/manual");
        fs::create_dir_all(&manual_skill).unwrap();
        fs::write(manual_skill.join("SKILL.md"), "---\nname: Manual\n---\n").unwrap();
        let vault = Arc::new(
            OpenVault::open(
                &directory.path().join("vault"),
                &directory.path().join("support"),
                &[],
            )
            .unwrap(),
        );
        let service = WorkspaceService::new(
            vault.repositories.clone(),
            vault.paths.root().to_path_buf(),
            Arc::new(Mutex::new(WatchCoordinator::default())),
            Arc::new(Mutex::new(())),
        );
        let before = fs::read(skill.join("SKILL.md")).unwrap();

        let manual = service
            .add_manual_project(ManualProjectAddRequest {
                selected_path: manual_project.to_string_lossy().into_owned(),
            })
            .unwrap();
        assert!(!manual.git);
        let manual_scan = service
            .rescan_manual_project(ManualProjectIdRequest {
                project_id: manual.project_id,
            })
            .unwrap();
        assert!(manual_scan.complete);
        assert!(manual_scan.skill_count > 0);

        let added = service
            .add(WorkspaceRootAddRequest {
                selected_path: workspace.to_string_lossy().into_owned(),
                maximum_depth: None,
                ignore_rules: Vec::new(),
            })
            .unwrap();
        let paused = service
            .pause(WorkspaceRootPauseRequest {
                root_id: added.root_id.clone(),
                paused: true,
            })
            .unwrap();
        assert!(paused.paused);
        assert!(matches!(
            service.rescan(WorkspaceRootIdRequest {
                root_id: added.root_id.clone()
            }),
            Err(WorkspaceError::RootPaused)
        ));
        service
            .pause(WorkspaceRootPauseRequest {
                root_id: added.root_id.clone(),
                paused: false,
            })
            .unwrap();

        let first = service
            .rescan(WorkspaceRootIdRequest {
                root_id: added.root_id.clone(),
            })
            .unwrap();
        let second = service
            .rescan(WorkspaceRootIdRequest {
                root_id: added.root_id.clone(),
            })
            .unwrap();
        assert!(first.complete);
        assert_eq!(first.project_count, 1);
        assert!(first.skill_count > 0);
        assert_eq!(first.skill_count, second.skill_count);
        assert_eq!(fs::read(skill.join("SKILL.md")).unwrap(), before);

        let original_workspace = directory.path().join("original-workspace");
        fs::rename(&workspace, &original_workspace).unwrap();
        fs::create_dir(&workspace).unwrap();
        assert_eq!(service.list().unwrap()[0].coverage_state, "stale");
        let replaced = service
            .rescan(WorkspaceRootIdRequest {
                root_id: added.root_id.clone(),
            })
            .unwrap();
        assert!(!replaced.complete);

        let removed = service
            .remove(WorkspaceRootIdRequest {
                root_id: added.root_id,
            })
            .unwrap();
        assert!(removed.removed);
        assert!(
            original_workspace
                .join(".agents/skills/example/SKILL.md")
                .is_file()
        );
        assert!(service.list().unwrap().is_empty());
    }
}
