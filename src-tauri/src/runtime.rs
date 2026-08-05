//! Long-lived runtime services managed by Tauri.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    },
    operations::{OperationCoordinator, OperationStore},
    persistence::{
        OpenVault, VaultError, default_application_support, default_vault_path,
        existing_device_settings,
    },
};

const DEFAULT_BLOCKING_WORKER_LIMIT: u8 = 4;

#[derive(Clone)]
pub struct AppRuntime {
    blocking_work: BlockingWorkPool,
    home: Option<PathBuf>,
    scan_jobs: ScanJobs,
    services: Arc<Mutex<Option<RuntimeServices>>>,
}

struct RuntimeServices {
    vault: Arc<OpenVault>,
    takeover: Arc<TakeoverService>,
    deployment: Arc<DeploymentService>,
    activity: Arc<ActivityService>,
    startup_recovery: Result<StartupRecoveryReport, String>,
}

impl AppRuntime {
    pub fn foundation() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::foundation_for_home(home)
    }

    fn foundation_for_home(home: Option<PathBuf>) -> Self {
        let blocking_work = BlockingWorkPool::new(DEFAULT_BLOCKING_WORKER_LIMIT);
        let active_vault = home
            .as_deref()
            .filter(|path| path.is_absolute())
            .and_then(open_configured_vault);
        let services = active_vault.and_then(|vault| RuntimeServices::install(vault).ok());
        Self {
            blocking_work,
            home,
            scan_jobs: ScanJobs::default(),
            services: Arc::new(Mutex::new(services)),
        }
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
        self.blocking_work.run(work)
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

    pub(crate) fn activity_service(&self) -> Result<Arc<ActivityService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        Ok(self.services()?.activity)
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
        match &self.services()?.startup_recovery {
            Ok(report) if report.completed => Ok(()),
            Ok(_) | Err(_) => Err(RuntimeStateError::StartupRecoveryFailed),
        }
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
            coordinator,
        ));
        let store = OperationStore::open(vault.paths.manager())?;
        let activity = Arc::new(ActivityService::new(vault.repositories.clone(), store));
        let mut services = Self {
            vault,
            takeover,
            deployment,
            activity,
            startup_recovery: Ok(StartupRecoveryReport::default()),
        };
        services.startup_recovery = services.run_startup_recovery();
        Ok(services)
    }

    fn snapshot(&self) -> RuntimeServicesSnapshot {
        RuntimeServicesSnapshot {
            vault: Arc::clone(&self.vault),
            takeover: Arc::clone(&self.takeover),
            deployment: Arc::clone(&self.deployment),
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
        let store =
            OperationStore::open(vault.paths.manager()).map_err(|error| error.to_string())?;
        let ids = store
            .nonterminal_operation_ids()
            .map_err(|error| error.to_string())?;
        let mut operations = Vec::with_capacity(ids.len());
        for id in ids {
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
                kind => Err(format!("unsupported recovery operation kind: {kind:?}")),
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
        })
    }
}

#[derive(Clone)]
struct RuntimeServicesSnapshot {
    vault: Arc<OpenVault>,
    takeover: Arc<TakeoverService>,
    deployment: Arc<DeploymentService>,
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
}

#[derive(Debug, Clone, Default, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct StartupRecoveryReport {
    pub completed: bool,
    pub operations: Vec<StartupRecoveryEvidence>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppErrorCode, AppErrorView};

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
}
