//! Long-lived runtime services managed by Tauri.

use std::{
    collections::BTreeSet,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Serialize;
use specta::Type;
use thiserror::Error;
use tokio::sync::{AcquireError, Semaphore};

use crate::{
    adapters::universal_global_root,
    application::{
        activity::ActivityService,
        deployment::DeploymentService,
        scanning::{ScanJobs, ScanningService},
        takeover::TakeoverService,
        trash::TrashService,
        vault_lifecycle::{LifecycleRecoveryEvidence, VaultLifecycleService},
        workspaces::WorkspaceService,
    },
    diagnostics::{
        DiagnosticsError, DiagnosticsExport, DiagnosticsSaveResult, DiagnosticsService,
        DiagnosticsStatus,
    },
    operations::{OperationCoordinator, OperationStore},
    persistence::{
        OpenVault, VaultError, default_application_support, default_vault_path,
        existing_device_settings, update_debug_logging,
    },
    scanner::{NotifyBackend, ReconcileReason, WatchBackend, WatchCoordinator, WatchEvent},
};

const DEFAULT_BLOCKING_WORKER_LIMIT: u8 = 4;

#[derive(Clone)]
pub struct AppRuntime {
    blocking_work: BlockingWorkPool,
    home: Option<PathBuf>,
    scan_jobs: ScanJobs,
    watch_coordinator: Arc<Mutex<WatchCoordinator>>,
    workspace_gate: Arc<Mutex<()>>,
    services: Arc<Mutex<Option<RuntimeServices>>>,
    restart_required: Arc<AtomicBool>,
    diagnostics: Result<DiagnosticsService, String>,
}

struct RuntimeServices {
    vault: Arc<OpenVault>,
    coordinator: Arc<OperationCoordinator>,
    takeover: Arc<TakeoverService>,
    deployment: Arc<DeploymentService>,
    trash: Arc<TrashService>,
    activity: Arc<ActivityService>,
    startup_recovery: Result<StartupRecoveryReport, String>,
}

impl AppRuntime {
    pub fn foundation() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut diagnostics = diagnostics_for_home(home.as_ref());
        if let Some(service) = diagnostics.as_ref().ok().cloned() {
            use tracing_subscriber::prelude::*;
            if tracing::subscriber::set_global_default(
                tracing_subscriber::registry().with(service.layer()),
            )
            .is_err()
            {
                diagnostics = Err("diagnostics subscriber unavailable".to_owned());
            }
        }
        Self::foundation_with_diagnostics(home, diagnostics)
    }

    #[cfg(test)]
    fn foundation_for_home(home: Option<PathBuf>) -> Self {
        let diagnostics = diagnostics_for_home(home.as_ref());
        Self::foundation_with_diagnostics(home, diagnostics)
    }

    fn foundation_with_diagnostics(
        home: Option<PathBuf>,
        diagnostics: Result<DiagnosticsService, String>,
    ) -> Self {
        let blocking_work = BlockingWorkPool::new(DEFAULT_BLOCKING_WORKER_LIMIT);
        let active_vault = home
            .as_deref()
            .filter(|path| path.is_absolute())
            .and_then(open_configured_vault);
        let services = active_vault.and_then(|vault| RuntimeServices::install(vault).ok());
        let runtime = Self {
            blocking_work,
            home,
            scan_jobs: ScanJobs::default(),
            watch_coordinator: Arc::new(Mutex::new(WatchCoordinator::default())),
            workspace_gate: Arc::new(Mutex::new(())),
            services: Arc::new(Mutex::new(services)),
            restart_required: Arc::new(AtomicBool::new(false)),
            diagnostics,
        };
        if let Ok(service) = runtime.workspace_service() {
            let _ = service.initialize_reconciliation();
        }
        runtime
    }

    pub(crate) fn initialize_vault(
        &self,
        selected_root: Option<PathBuf>,
    ) -> Result<RuntimeVaultSummary, VaultInitializationError> {
        let home = self
            .home
            .as_deref()
            .filter(|path| path.is_absolute())
            .ok_or(RuntimeStateError::HomeUnavailable)?;
        let mut services = self
            .services
            .lock()
            .map_err(|_| RuntimeStateError::StatePoisoned)?;
        if services.is_some() {
            return Err(RuntimeStateError::VaultAlreadyInitialized.into());
        }
        let selected_root = selected_root.unwrap_or_else(|| default_vault_path(home));
        let application_support = default_application_support(home);
        let mut configured_targets = vec![universal_global_root(home).root];
        if let Some(settings) = existing_device_settings(&application_support)? {
            configured_targets.extend(settings.target_overrides.into_values());
            configured_targets.extend(settings.custom_target_paths);
        }
        let installed = RuntimeServices::install(Arc::new(OpenVault::open(
            &selected_root,
            &application_support,
            &configured_targets,
        )?))?;
        let summary = installed.summary();
        *services = Some(installed);
        self.restart_required.store(false, Ordering::Release);
        drop(services);
        if let Ok(service) = self.workspace_service() {
            let _ = service.initialize_reconciliation();
        }
        Ok(summary)
    }

    pub(crate) fn vault_status(&self) -> Result<Option<RuntimeVaultStatus>, RuntimeStateError> {
        let services = self
            .services
            .lock()
            .map_err(|_| RuntimeStateError::StatePoisoned)?;
        Ok(services.as_ref().map(|services| RuntimeVaultStatus {
            summary: services.summary(),
            startup_recovery_completed: services
                .startup_recovery
                .as_ref()
                .ok()
                .map(|report| report.completed),
        }))
    }

    pub const fn blocking_worker_limit(&self) -> u8 {
        self.blocking_work.limit
    }

    pub(crate) fn home_path(&self) -> Result<PathBuf, RuntimeStateError> {
        self.home
            .as_ref()
            .filter(|path| path.is_absolute())
            .cloned()
            .ok_or(RuntimeStateError::HomeUnavailable)
    }

    pub fn run_blocking<F, T>(&self, work: F) -> impl Future<Output = Result<T, BlockingWorkError>>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        let span = tracing::Span::current();
        self.blocking_work.run(move || {
            tracing::dispatcher::with_default(&dispatcher, || {
                let _guard = span.enter();
                work()
            })
        })
    }

    pub(crate) fn diagnostics_status(&self) -> Result<DiagnosticsStatus, DiagnosticsError> {
        match &self.diagnostics {
            Ok(service) => service.status(),
            Err(_) => Ok(DiagnosticsStatus {
                available: false,
                debug_logging: false,
                blocked: false,
                level: "unavailable".into(),
                health: "unavailable".into(),
                managed_bytes: "0".into(),
                segment_count: 0,
                dropped_record_count: "0".into(),
            }),
        }
    }
    pub(crate) fn diagnostics_debug_set(
        &self,
        enabled: bool,
    ) -> Result<DiagnosticsStatus, DiagnosticsError> {
        let service = self
            .diagnostics
            .as_ref()
            .map_err(|_| DiagnosticsError::Unavailable)?;
        let home = self.home.as_deref().ok_or(DiagnosticsError::Unavailable)?;
        update_debug_logging(&default_application_support(home), enabled)
            .map_err(|_| DiagnosticsError::Unavailable)?;
        service.set_debug(enabled);
        service.status()
    }
    pub(crate) fn diagnostics_export_prepare(&self) -> Result<DiagnosticsExport, DiagnosticsError> {
        self.diagnostics
            .as_ref()
            .map_err(|_| DiagnosticsError::Unavailable)?
            .prepare()
    }
    pub(crate) fn diagnostics_export_save(
        &self,
        id: &str,
        digest: &str,
        path: &Path,
    ) -> Result<DiagnosticsSaveResult, DiagnosticsError> {
        self.diagnostics
            .as_ref()
            .map_err(|_| DiagnosticsError::Unavailable)?
            .save(id, digest, path)
    }

    pub(crate) fn scanning_service(&self) -> Result<ScanningService, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        let home = self
            .home
            .as_ref()
            .filter(|path| path.is_absolute())
            .cloned()
            .ok_or(RuntimeStateError::HomeUnavailable)?;
        let services = self.services()?;
        Ok(ScanningService::new(
            home,
            services.vault.repositories.clone(),
            self.blocking_work.clone(),
            self.scan_jobs.clone(),
        ))
    }

    pub(crate) fn takeover_service(&self) -> Result<Arc<TakeoverService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        Ok(self.services()?.takeover)
    }

    pub(crate) fn deployment_service(&self) -> Result<Arc<DeploymentService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        Ok(self.services()?.deployment)
    }

    pub(crate) fn trash_service(&self) -> Result<Arc<TrashService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        Ok(self.services()?.trash)
    }

    pub(crate) fn activity_service(&self) -> Result<Arc<ActivityService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        Ok(self.services()?.activity)
    }

    pub(crate) fn vault_lifecycle_service(
        &self,
    ) -> Result<VaultLifecycleService, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        let services = self.services()?;
        Ok(VaultLifecycleService::with_runtime(
            services.vault,
            services.coordinator,
            default_application_support(&self.home_path()?),
        ))
    }

    pub(crate) fn workspace_service(&self) -> Result<WorkspaceService, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        let services = self.services()?;
        Ok(WorkspaceService::new(
            services.vault.repositories.clone(),
            services.vault.paths.root().to_path_buf(),
            Arc::clone(&self.watch_coordinator),
            Arc::clone(&self.workspace_gate),
        ))
    }

    pub(crate) fn request_workspace_reconciliation(&self, reason: ReconcileReason) {
        if let Ok(mut coordinator) = self.watch_coordinator.lock() {
            coordinator.proactive(reason);
        }
    }

    pub(crate) fn start_workspace_reconciliation(&self) {
        let runtime = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut backend = NotifyBackend::new().ok();
            let mut watched = BTreeSet::new();
            let mut ticks = 0_u16;
            loop {
                if backend.is_none() {
                    backend = NotifyBackend::new().ok();
                    watched.clear();
                }
                let boundaries = runtime
                    .watch_coordinator
                    .lock()
                    .map(|coordinator| coordinator.boundaries())
                    .unwrap_or_default();
                let desired = boundaries.into_iter().collect::<BTreeSet<_>>();
                if let Some(active) = backend.as_mut() {
                    for removed in watched.difference(&desired).cloned().collect::<Vec<_>>() {
                        let _ = active.unwatch(&removed);
                        watched.remove(&removed);
                    }
                    for added in desired.difference(&watched).cloned().collect::<Vec<_>>() {
                        if active.watch(&added).is_ok() {
                            watched.insert(added);
                        } else if let Ok(mut coordinator) = runtime.watch_coordinator.lock() {
                            coordinator.ingest(WatchEvent::CoverageLost(added));
                        }
                    }
                }

                let mut disconnected = false;
                if let Some(active) = backend.as_mut() {
                    for _ in 0..1_024 {
                        let Some(event) = active.try_event() else {
                            break;
                        };
                        disconnected = event == WatchEvent::Disconnected;
                        if let Ok(mut coordinator) = runtime.watch_coordinator.lock() {
                            coordinator.ingest(event);
                        }
                        if disconnected {
                            break;
                        }
                    }
                }
                if disconnected {
                    backend = NotifyBackend::new().ok();
                    watched.clear();
                }

                ticks = ticks.saturating_add(1);
                if ticks >= 120 {
                    runtime.request_workspace_reconciliation(ReconcileReason::Wake);
                    let _ = runtime.diagnostics_status();
                    ticks = 0;
                }
                let request = runtime
                    .watch_coordinator
                    .lock()
                    .ok()
                    .and_then(|mut coordinator| coordinator.drain());
                if let Some(request) = request {
                    let retry = match runtime.workspace_service() {
                        Ok(service) => {
                            let work_request = request.clone();
                            match runtime
                                .run_blocking(move || service.reconcile_request(work_request))
                                .await
                            {
                                Ok(results) => results.iter().any(Result::is_err),
                                Err(_) => true,
                            }
                        }
                        Err(_) => true,
                    };
                    if retry && let Ok(mut coordinator) = runtime.watch_coordinator.lock() {
                        coordinator.requeue(request);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
    }

    pub(crate) fn startup_recovery_status(
        &self,
    ) -> Result<StartupRecoveryReport, RuntimeStateError> {
        self.services()?
            .startup_recovery
            .clone()
            .map_err(|_| RuntimeStateError::StartupRecoveryFailed)
    }

    fn ensure_startup_recovery(&self) -> Result<(), RuntimeStateError> {
        if self.restart_required.load(Ordering::Acquire) {
            return Err(RuntimeStateError::RestartRequired);
        }
        match &self.services()?.startup_recovery {
            Ok(report) if report.completed => Ok(()),
            Ok(_) | Err(_) => Err(RuntimeStateError::StartupRecoveryFailed),
        }
    }

    pub(crate) fn enter_restart_required(&self) {
        self.restart_required.store(true, Ordering::Release);
    }

    fn services(&self) -> Result<RuntimeServicesSnapshot, RuntimeStateError> {
        let services = self
            .services
            .lock()
            .map_err(|_| RuntimeStateError::StatePoisoned)?;
        services
            .as_ref()
            .map(RuntimeServices::snapshot)
            .ok_or(RuntimeStateError::VaultNotInitialized)
    }
}

impl RuntimeServices {
    fn install(vault: Arc<OpenVault>) -> Result<Self, VaultInitializationError> {
        let coordinator = Arc::new(OperationCoordinator::new());
        let takeover = Arc::new(TakeoverService::with_runtime(
            Arc::clone(&vault),
            Arc::clone(&coordinator),
        ));
        let deployment = Arc::new(DeploymentService::with_runtime(
            Arc::clone(&vault),
            Arc::clone(&coordinator),
        ));
        let trash = Arc::new(
            TrashService::with_runtime(Arc::clone(&vault), Arc::clone(&coordinator))
                .map_err(|error| VaultInitializationError::StartupRecovery(error.to_string()))?,
        );
        let store = OperationStore::open(vault.paths.manager())?;
        let activity = Arc::new(ActivityService::new(vault.repositories.clone(), store));
        let mut services = Self {
            vault,
            coordinator,
            takeover,
            deployment,
            trash,
            activity,
            startup_recovery: Ok(StartupRecoveryReport::default()),
        };
        services.startup_recovery = services.run_startup_recovery();
        Ok(services)
    }

    fn snapshot(&self) -> RuntimeServicesSnapshot {
        RuntimeServicesSnapshot {
            vault: Arc::clone(&self.vault),
            coordinator: Arc::clone(&self.coordinator),
            takeover: Arc::clone(&self.takeover),
            deployment: Arc::clone(&self.deployment),
            trash: Arc::clone(&self.trash),
            activity: Arc::clone(&self.activity),
            startup_recovery: self.startup_recovery.clone(),
        }
    }

    fn summary(&self) -> RuntimeVaultSummary {
        RuntimeVaultSummary {
            root_path: self.vault.paths.root().to_path_buf(),
            vault_id: self.vault.manifest.vault_id.to_string(),
        }
    }

    fn run_startup_recovery(&self) -> Result<StartupRecoveryReport, String> {
        let vault = &self.vault;
        let lifecycle = VaultLifecycleService::with_runtime(
            Arc::clone(vault),
            Arc::clone(&self.coordinator),
            PathBuf::new(),
        )
        .recover_startup()
        .map_err(|error| error.to_string())?;
        if !lifecycle.completed {
            return Ok(StartupRecoveryReport {
                completed: false,
                operations: Vec::new(),
                lifecycle_operations: lifecycle.operations,
            });
        }
        let store =
            OperationStore::open(vault.paths.manager()).map_err(|error| error.to_string())?;
        let ids = store
            .nonterminal_operation_ids()
            .map_err(|error| error.to_string())?;
        let mut operations = Vec::with_capacity(ids.len());
        for id in ids {
            let operation_span = crate::diagnostics::operation_span(&id.to_string());
            let _operation_guard = operation_span.enter();
            tracing::info!(target: "skills_hub::recovery", "startup operation recovery began");
            let stored = store.load(id).map_err(|error| error.to_string())?;
            let result = match stored.plan.content.kind {
                crate::operations::OperationKind::TakeOver => self
                    .takeover
                    .recover_operation(id)
                    .map_err(|error| error.to_string()),
                crate::operations::OperationKind::Deploy
                | crate::operations::OperationKind::Undeploy
                | crate::operations::OperationKind::Undo => self
                    .deployment
                    .recover_operation(id)
                    .map_err(|error| error.to_string()),
                crate::operations::OperationKind::MoveToTrash
                | crate::operations::OperationKind::Restore
                | crate::operations::OperationKind::PermanentlyDelete => self
                    .trash
                    .recover_operation(id)
                    .map_err(|error| error.to_string()),
            };
            operations.push(match result {
                Ok(execution) => StartupRecoveryEvidence {
                    operation_id: id.to_string(),
                    status: format!("{:?}", execution.outcome).to_lowercase(),
                    outcome: Some(format!("{:?}", execution.outcome)),
                    error: None,
                },
                Err(error) => {
                    let outcome = store
                        .load(id)
                        .ok()
                        .and_then(|stored| stored.journal.outcome)
                        .map(|value| format!("{value:?}"));
                    let status = outcome
                        .as_deref()
                        .map_or_else(|| "error".into(), str::to_lowercase);
                    StartupRecoveryEvidence {
                        operation_id: id.to_string(),
                        status,
                        outcome,
                        error: Some(error),
                    }
                }
            });
        }
        self.activity
            .rebuild_terminal_operations()
            .map_err(|error| error.to_string())?;
        let completed = store
            .nonterminal_operation_ids()
            .map_err(|error| error.to_string())?
            .is_empty();
        Ok(StartupRecoveryReport {
            completed,
            operations,
            lifecycle_operations: lifecycle.operations,
        })
    }
}

#[derive(Clone)]
struct RuntimeServicesSnapshot {
    vault: Arc<OpenVault>,
    coordinator: Arc<OperationCoordinator>,
    takeover: Arc<TakeoverService>,
    deployment: Arc<DeploymentService>,
    trash: Arc<TrashService>,
    activity: Arc<ActivityService>,
    startup_recovery: Result<StartupRecoveryReport, String>,
}

#[derive(Debug)]
pub(crate) struct RuntimeVaultSummary {
    pub root_path: PathBuf,
    pub vault_id: String,
}

pub(crate) struct RuntimeVaultStatus {
    pub summary: RuntimeVaultSummary,
    pub startup_recovery_completed: Option<bool>,
}

#[derive(Debug, Error)]
pub enum VaultInitializationError {
    #[error(transparent)]
    Runtime(#[from] RuntimeStateError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("Vault operation store failed: {0}")]
    OperationStore(#[from] crate::operations::JournalError),
    #[error("startup recovery service failed: {0}")]
    StartupRecovery(String),
}

#[derive(Debug, Clone, Default, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StartupRecoveryReport {
    pub completed: bool,
    pub operations: Vec<StartupRecoveryEvidence>,
    pub lifecycle_operations: Vec<LifecycleRecoveryEvidence>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StartupRecoveryEvidence {
    pub operation_id: String,
    pub status: String,
    pub outcome: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct BlockingWorkPool {
    limit: u8,
    permits: Arc<Semaphore>,
}

impl BlockingWorkPool {
    pub(crate) fn new(limit: u8) -> Self {
        let limit = limit.max(1);
        Self {
            limit,
            permits: Arc::new(Semaphore::new(usize::from(limit))),
        }
    }

    pub(crate) async fn run<F, T>(&self, work: F) -> Result<T, BlockingWorkError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = Arc::clone(&self.permits).acquire_owned().await?;
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await?;

        Ok(result)
    }
}

fn diagnostics_for_home(home: Option<&PathBuf>) -> Result<DiagnosticsService, String> {
    home.filter(|path| path.is_absolute()).map_or_else(
        || Err("home unavailable".to_owned()),
        |home| {
            let support = default_application_support(home);
            let debug = existing_device_settings(&support)
                .ok()
                .flatten()
                .is_some_and(|settings| settings.debug_logging);
            DiagnosticsService::new(support.join("diagnostics"), Some(home.clone()), debug)
                .map_err(|error| error.to_string())
        },
    )
}

fn open_configured_vault(home: &Path) -> Option<Arc<OpenVault>> {
    let application_support = default_application_support(home);
    let settings = existing_device_settings(&application_support)
        .ok()
        .flatten()?;
    let mut configured_targets = vec![universal_global_root(home).root];
    configured_targets.extend(settings.target_overrides.into_values());
    configured_targets.extend(settings.custom_target_paths);
    OpenVault::open(
        &settings.active_vault_path,
        &application_support,
        &configured_targets,
    )
    .ok()
    .map(Arc::new)
}

#[derive(Debug, Error)]
pub enum BlockingWorkError {
    #[error("blocking worker semaphore closed")]
    PoolClosed(#[from] AcquireError),
    #[error("blocking worker task failed")]
    TaskFailed(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, Copy, Error)]
pub enum RuntimeStateError {
    #[error("the current user's home directory is unavailable")]
    HomeUnavailable,
    #[error("a Vault must be initialized before scanning")]
    VaultNotInitialized,
    #[error("a Vault is already initialized for this application session")]
    VaultAlreadyInitialized,
    #[error("runtime service state is unavailable")]
    StatePoisoned,
    #[error("startup recovery could not establish authoritative operation state")]
    StartupRecoveryFailed,
    #[error("Vault maintenance completed; restart is required before runtime services can be used")]
    RestartRequired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppErrorCode, AppErrorView};
    use tracing_subscriber::prelude::*;

    #[tokio::test]
    async fn blocking_work_returns_result_without_using_the_command_thread() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = AppRuntime::foundation_for_home(Some(directory.path().to_path_buf()));
        let result = runtime.run_blocking(|| 21 * 2).await.unwrap();

        assert_eq!(result, 42);
        assert_eq!(
            runtime.blocking_worker_limit(),
            DEFAULT_BLOCKING_WORKER_LIMIT
        );
    }

    #[tokio::test]
    async fn blocking_work_preserves_operation_diagnostics_context() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = AppRuntime::foundation_for_home(Some(directory.path().to_path_buf()));
        let diagnostics = runtime.diagnostics.as_ref().unwrap().clone();
        let subscriber = tracing_subscriber::registry().with(diagnostics.layer());
        let dispatch = tracing::Dispatch::new(subscriber);
        let operation_id = crate::domain::OperationId::generate().to_string();
        let work = tracing::dispatcher::with_default(&dispatch, || {
            let span = crate::diagnostics::operation_span(&operation_id);
            let _guard = span.enter();
            runtime.run_blocking(|| {
                tracing::info!(
                    target: "skills_hub::runtime",
                    event_code = "blocking_work_test"
                );
            })
        });

        work.await.unwrap();
        let export = diagnostics.prepare().unwrap();
        assert!(export.preview.contains(&operation_id));
        assert!(export.preview.contains("blocking_work_test"));
    }

    #[test]
    fn default_vault_initializes_services_and_updates_status() {
        let home = tempfile::tempdir().unwrap();
        let runtime = AppRuntime::foundation_for_home(Some(home.path().to_path_buf()));

        assert!(runtime.vault_status().unwrap().is_none());
        assert!(matches!(
            runtime.scanning_service(),
            Err(RuntimeStateError::VaultNotInitialized)
        ));

        let summary = runtime.initialize_vault(None).unwrap();
        assert_eq!(
            summary.root_path,
            default_vault_path(home.path()).canonicalize().unwrap()
        );
        assert!(!summary.vault_id.is_empty());
        assert!(summary.root_path.join(".manager/vault.json").is_file());

        let status = runtime.vault_status().unwrap().unwrap();
        assert_eq!(status.summary.root_path, summary.root_path);
        assert_eq!(status.startup_recovery_completed, Some(true));
        assert!(runtime.scanning_service().is_ok());
        assert!(runtime.takeover_service().is_ok());
        assert!(runtime.deployment_service().is_ok());
        assert!(runtime.activity_service().is_ok());
    }

    #[test]
    fn unsafe_custom_vault_errors_are_preserved_in_the_app_envelope() {
        let home = tempfile::tempdir().unwrap();
        let runtime = AppRuntime::foundation_for_home(Some(home.path().to_path_buf()));
        let relative: AppErrorView = runtime
            .initialize_vault(Some(PathBuf::from("relative-vault")))
            .unwrap_err()
            .into();
        assert!(matches!(relative.code, AppErrorCode::UnsafePath));

        let nested_runtime = AppRuntime::foundation_for_home(Some(home.path().to_path_buf()));
        let nested: AppErrorView = nested_runtime
            .initialize_vault(Some(home.path().join(".agents/skills/nested-vault")))
            .unwrap_err()
            .into();
        assert!(matches!(nested.code, AppErrorCode::UnsafePath));
    }

    #[test]
    fn active_vault_cannot_be_reinitialized() {
        let home = tempfile::tempdir().unwrap();
        let runtime = AppRuntime::foundation_for_home(Some(home.path().to_path_buf()));
        runtime.initialize_vault(None).unwrap();

        assert!(matches!(
            runtime.initialize_vault(None),
            Err(VaultInitializationError::Runtime(
                RuntimeStateError::VaultAlreadyInitialized
            ))
        ));
    }

    #[test]
    fn restart_required_state_blocks_every_runtime_service() {
        let home = tempfile::tempdir().unwrap();
        let runtime = AppRuntime::foundation_for_home(Some(home.path().to_path_buf()));
        runtime.initialize_vault(None).unwrap();
        runtime.enter_restart_required();

        assert!(matches!(
            runtime.scanning_service(),
            Err(RuntimeStateError::RestartRequired)
        ));
        assert!(matches!(
            runtime.takeover_service(),
            Err(RuntimeStateError::RestartRequired)
        ));
        assert!(matches!(
            runtime.deployment_service(),
            Err(RuntimeStateError::RestartRequired)
        ));
        assert!(matches!(
            runtime.activity_service(),
            Err(RuntimeStateError::RestartRequired)
        ));
        assert!(matches!(
            runtime.vault_lifecycle_service(),
            Err(RuntimeStateError::RestartRequired)
        ));
        assert!(matches!(
            runtime.workspace_service(),
            Err(RuntimeStateError::RestartRequired)
        ));
    }
}
