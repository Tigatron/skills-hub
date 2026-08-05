//! Long-lived runtime services managed by Tauri.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
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
    persistence::{OpenVault, default_application_support, existing_device_settings},
};

const DEFAULT_BLOCKING_WORKER_LIMIT: u8 = 4;

#[derive(Clone)]
pub struct AppRuntime {
    blocking_work: BlockingWorkPool,
    home: Option<PathBuf>,
    active_vault: Option<Arc<OpenVault>>,
    scan_jobs: ScanJobs,
    takeover: Option<Arc<TakeoverService>>,
    deployment: Option<Arc<DeploymentService>>,
    activity: Option<Arc<ActivityService>>,
    startup_recovery: Arc<OnceLock<Result<StartupRecoveryReport, String>>>,
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
        let coordinator = Arc::new(OperationCoordinator::new());
        let takeover = active_vault.as_ref().map(|vault| {
            Arc::new(TakeoverService::with_runtime(
                Arc::clone(vault),
                Arc::clone(&coordinator),
            ))
        });
        let deployment = active_vault.as_ref().map(|vault| {
            Arc::new(DeploymentService::with_runtime(
                Arc::clone(vault),
                Arc::clone(&coordinator),
            ))
        });
        let activity = active_vault.as_ref().and_then(|vault| {
            OperationStore::open(vault.paths.manager())
                .ok()
                .map(|store| Arc::new(ActivityService::new(vault.repositories.clone(), store)))
        });
        let runtime = Self {
            blocking_work,
            home,
            active_vault,
            scan_jobs: ScanJobs::default(),
            takeover,
            deployment,
            activity,
            startup_recovery: Arc::new(OnceLock::new()),
        };
        if runtime.active_vault.is_some() {
            let _ = runtime.startup_recovery.set(runtime.run_startup_recovery());
        }
        runtime
    }

    pub const fn blocking_worker_limit(&self) -> u8 {
        self.blocking_work.limit
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
        let vault = self
            .active_vault
            .as_ref()
            .ok_or(RuntimeStateError::VaultNotInitialized)?;
        Ok(ScanningService::new(
            home,
            vault.repositories.clone(),
            self.blocking_work.clone(),
            self.scan_jobs.clone(),
        ))
    }

    pub(crate) fn takeover_service(&self) -> Result<Arc<TakeoverService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        self.takeover
            .clone()
            .ok_or(RuntimeStateError::VaultNotInitialized)
    }

    pub(crate) fn deployment_service(&self) -> Result<Arc<DeploymentService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        self.deployment
            .clone()
            .ok_or(RuntimeStateError::VaultNotInitialized)
    }

    pub(crate) fn activity_service(&self) -> Result<Arc<ActivityService>, RuntimeStateError> {
        self.ensure_startup_recovery()?;
        self.activity
            .clone()
            .ok_or(RuntimeStateError::VaultNotInitialized)
    }

    pub(crate) fn startup_recovery_status(
        &self,
    ) -> Result<StartupRecoveryReport, RuntimeStateError> {
        self.startup_recovery
            .get_or_init(|| self.run_startup_recovery())
            .clone()
            .map_err(|_| RuntimeStateError::StartupRecoveryFailed)
    }

    fn ensure_startup_recovery(&self) -> Result<(), RuntimeStateError> {
        if self.active_vault.is_none() {
            return Ok(());
        }
        let result = self
            .startup_recovery
            .get_or_init(|| self.run_startup_recovery());
        match result {
            Ok(report) if report.completed => Ok(()),
            Ok(_) | Err(_) => Err(RuntimeStateError::StartupRecoveryFailed),
        }
    }

    fn run_startup_recovery(&self) -> Result<StartupRecoveryReport, String> {
        let Some(vault) = &self.active_vault else {
            return Ok(StartupRecoveryReport::default());
        };
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
                    .as_ref()
                    .expect("configured")
                    .recover_operation(id)
                    .map_err(|error| error.to_string()),
                crate::operations::OperationKind::Deploy
                | crate::operations::OperationKind::Undeploy
                | crate::operations::OperationKind::Undo => self
                    .deployment
                    .as_ref()
                    .expect("configured")
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
        if let Some(activity) = &self.activity {
            activity
                .rebuild_terminal_operations()
                .map_err(|error| error.to_string())?;
        }
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
    #[error("startup recovery could not establish authoritative operation state")]
    StartupRecoveryFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
