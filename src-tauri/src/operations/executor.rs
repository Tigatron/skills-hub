use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
};

use rustix::fs::{RenameFlags, renameat_with};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    domain::{OperationId, OperationOutcome, OperationState, TargetId, UtcTimestamp},
    filesystem::durable::sync_directory,
    filesystem::{
        AuthorizedPath, AuthorizedRoot, BundleCaps, EntryKind, MetadataFingerprint, PathIdentity,
        hash_bundle,
    },
};

use super::{
    JournalError, OperationFailure, OperationJournal, OperationPlan, OperationStore,
    PathFingerprint, PhaseEvidence, PlanAction, PlanDigest, PlanPath, PlanStep, SnapshotProtection,
    StartupDecision, StoredOperation, classify_startup,
};

/// Cooperative cancellation checked only before active-path commit begins.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn check(&self) -> Result<(), OperationError> {
        if self.is_cancelled() {
            Err(OperationError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Serializes all mutation execution for one open Vault.
#[derive(Debug, Default)]
pub struct OperationCoordinator {
    gate: Mutex<()>,
}

impl OperationCoordinator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gate: Mutex::new(()),
        }
    }

    fn acquire(&self) -> Result<MutexGuard<'_, ()>, OperationError> {
        match self.gate.try_lock() {
            Ok(guard) => Ok(guard),
            Err(TryLockError::WouldBlock) => Err(OperationError::MutationBusy),
            Err(TryLockError::Poisoned(_)) => Err(OperationError::CoordinatorUnavailable),
        }
    }

    /// Runs a Vault-root lifecycle transaction under the same single-writer gate as target
    /// mutations. Lifecycle actions have their own exact-path journal because they cannot be
    /// represented as target replacement steps.
    pub(crate) fn run_lifecycle<T, E>(&self, action: impl FnOnce() -> Result<T, E>) -> Result<T, E>
    where
        E: From<OperationError>,
    {
        let _guard = self.acquire().map_err(E::from)?;
        action()
    }
}

/// Authorized roots keyed only by durable target identity.
#[derive(Debug, Clone, Default)]
pub struct TargetRoots {
    roots: BTreeMap<TargetId, AuthorizedRoot>,
}

impl TargetRoots {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            roots: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, target_id: TargetId, root: AuthorizedRoot) {
        self.roots.insert(target_id, root);
    }

    pub(crate) fn authorize(&self, path: &PlanPath) -> Result<AuthorizedPath, OperationError> {
        let root = self
            .roots
            .get(&path.target_id())
            .ok_or(OperationError::UnknownTarget(path.target_id()))?;
        let root_metadata = fs::symlink_metadata(root.canonical_path()).map_err(|source| {
            OperationError::Filesystem {
                context: "revalidating authorized target root",
                source,
            }
        })?;
        let current_identity = MetadataFingerprint::from_metadata(&root_metadata);
        let expected_identity = root.identity();
        if root_metadata.file_type().is_symlink()
            || !root_metadata.is_dir()
            || current_identity.device_id != expected_identity.device_id
            || current_identity.file_id != expected_identity.file_id
            || current_identity.kind != expected_identity.kind
        {
            return Err(OperationError::StalePlan {
                step: None,
                detail: "authorized target root identity changed".to_owned(),
            });
        }
        let authorized =
            root.authorize(path.relative())
                .map_err(|error| OperationError::StalePlan {
                    step: None,
                    detail: error.to_string(),
                })?;
        let display = authorized.path().to_str().ok_or_else(|| {
            OperationError::InvalidPlan("authorized target path is not UTF-8".to_owned())
        })?;
        if display != path.display_path() {
            return Err(OperationError::StalePlan {
                step: None,
                detail: "authorized target path no longer matches the reviewed path".to_owned(),
            });
        }
        let parent_identity =
            authorized
                .parent_identity()
                .map_err(|error| OperationError::StalePlan {
                    step: None,
                    detail: format!("authorized final parent is unavailable or unsafe: {error}"),
                })?;
        if parent_identity != path.parent_identity() {
            return Err(OperationError::StalePlan {
                step: None,
                detail: "authorized final parent identity changed".to_owned(),
            });
        }
        Ok(authorized)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotRegistration {
    pub protections: Vec<SnapshotProtection>,
}

/// Publishes a protected operation-level recovery point before staging.
pub trait SnapshotRegistrar: Send + Sync {
    /// Implementations must be durable and idempotent by Operation ID.
    ///
    /// # Errors
    ///
    /// Returns an error when every required before-version was not protected.
    fn register(
        &self,
        plan: &OperationPlan,
        protected_steps: &[PlanStep],
        cancellation: &CancellationToken,
    ) -> Result<SnapshotRegistration, OperationHookError>;
}

/// Revalidates domain-owned invariants while holding the Vault mutation coordinator.
pub trait OperationPreflight: Send + Sync {
    /// # Errors
    /// Returns an error when domain state no longer matches the reviewed plan.
    fn preflight(&self, _plan: &OperationPlan) -> Result<(), OperationHookError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopOperationPreflight;
impl OperationPreflight for NoopOperationPreflight {}

/// Builds one operation-specific staged result at a kernel-authorized sibling path.
pub trait StagingProvider: Send + Sync {
    /// The provider must create `staging_path` exclusively, durably build the requested result,
    /// and never mutate the final path.
    ///
    /// # Errors
    ///
    /// Returns an error when staging cannot be completed and verified by the caller.
    fn stage(
        &self,
        plan: &OperationPlan,
        step: &PlanStep,
        staging_path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), OperationHookError>;

    /// Revalidates product-owned authority and capability evidence immediately before an
    /// active-path commit. Implementations must not mutate the final path.
    ///
    /// # Errors
    ///
    /// Returns an error when a reviewed product precondition changed after staging.
    fn revalidate_before_commit(
        &self,
        _plan: &OperationPlan,
        _step: &PlanStep,
    ) -> Result<(), OperationHookError> {
        Ok(())
    }
}

/// Idempotent metadata publication performed only after all final paths verify.
pub trait OperationFinalizer: Send + Sync {
    /// Publishes readable manifests atomically.
    ///
    /// # Errors
    ///
    /// Returns an error without undoing already verified active paths.
    fn publish_manifests(
        &self,
        plan: &OperationPlan,
        journal: &OperationJournal,
    ) -> Result<(), OperationHookError>;

    /// Finalizes `SQLite` Operation/Snapshot/Activity projections in one critical transaction.
    ///
    /// # Errors
    ///
    /// Returns an error without undoing already verified active paths.
    fn finalize_projection(
        &self,
        plan: &OperationPlan,
        journal: &OperationJournal,
    ) -> Result<(), OperationHookError>;
}

pub trait OperationEventSink: Send + Sync {
    fn publish(&self, event: OperationEvent);
}

pub trait OperationFailpoints: Send + Sync {
    /// # Errors
    ///
    /// Test implementations return an injected boundary failure.
    fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError>;
}

#[derive(Debug, Default)]
pub struct NoopOperationEventSink;

impl OperationEventSink for NoopOperationEventSink {
    fn publish(&self, _event: OperationEvent) {}
}

#[derive(Debug, Default)]
pub struct NoopOperationFailpoints;

impl OperationFailpoints for NoopOperationFailpoints {
    fn check(&self, _boundary: OperationBoundary) -> Result<(), OperationHookError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationBoundary {
    Preflighted,
    SnapshotPublished,
    StageIntentPersisted(u32),
    StageActionApplied(u32),
    StageObserved(u32),
    CommitIntentPersisted(u32),
    BackupRenamed(u32),
    FinalRenamed(u32),
    CommitObserved(u32),
    VerifyIntentPersisted(u32),
    VerifyObserved(u32),
    ManifestsPublished,
    ProjectionFinalized,
    JournalFinalized,
    RollbackIntentPersisted(u32),
    RollbackAsideRenamed(u32),
    RollbackActionApplied(u32),
    RollbackObserved(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationEvent {
    Progress {
        operation_id: OperationId,
        state: OperationState,
        step: Option<u32>,
    },
    Terminal {
        operation_id: OperationId,
        outcome: OperationOutcome,
    },
    Invalidated {
        operation_id: OperationId,
        resources: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationExecution {
    pub operation_id: OperationId,
    pub state: OperationState,
    pub outcome: OperationOutcome,
    pub failure: Option<OperationFailure>,
    pub cleanup_failures: Vec<String>,
    pub replayed: bool,
}

/// Generic, single-owner compensating transaction executor.
type RecoverPendingItem = (OperationId, Result<OperationExecution, OperationError>);
type RecoverPendingResult = Result<Vec<RecoverPendingItem>, OperationError>;

pub struct OperationExecutor {
    store: OperationStore,
    coordinator: Arc<OperationCoordinator>,
    roots: TargetRoots,
    stager: Arc<dyn StagingProvider>,
    snapshots: Arc<dyn SnapshotRegistrar>,
    finalizer: Arc<dyn OperationFinalizer>,
    domain_preflight: Arc<dyn OperationPreflight>,
    events: Arc<dyn OperationEventSink>,
    failpoints: Arc<dyn OperationFailpoints>,
}

impl OperationExecutor {
    #[must_use]
    pub fn new(
        store: OperationStore,
        coordinator: Arc<OperationCoordinator>,
        roots: TargetRoots,
        stager: Arc<dyn StagingProvider>,
        snapshots: Arc<dyn SnapshotRegistrar>,
        finalizer: Arc<dyn OperationFinalizer>,
    ) -> Self {
        Self {
            store,
            coordinator,
            roots,
            stager,
            snapshots,
            finalizer,
            domain_preflight: Arc::new(NoopOperationPreflight),
            events: Arc::new(NoopOperationEventSink),
            failpoints: Arc::new(NoopOperationFailpoints),
        }
    }

    #[must_use]
    pub fn with_preflight(mut self, preflight: Arc<dyn OperationPreflight>) -> Self {
        self.domain_preflight = preflight;
        self
    }

    #[must_use]
    pub fn with_event_sink(mut self, events: Arc<dyn OperationEventSink>) -> Self {
        self.events = events;
        self
    }

    #[must_use]
    pub fn with_failpoints(mut self, failpoints: Arc<dyn OperationFailpoints>) -> Self {
        self.failpoints = failpoints;
        self
    }

    /// Executes the exact persisted plan identified by Operation ID and confirmation digest.
    ///
    /// Replaying any terminal journal returns its recorded result without touching a target.
    /// Cancellation is checked only through preflight, snapshot, and stage-all.
    ///
    /// # Errors
    ///
    /// Returns a typed error for stale/expired plans, no-write failures, compensated failures,
    /// recovery-required ambiguity, or infrastructure durability failures.
    #[allow(clippy::too_many_lines)]
    pub fn execute(
        &self,
        operation_id: OperationId,
        plan_digest: PlanDigest,
        cancellation: &CancellationToken,
    ) -> Result<OperationExecution, OperationError> {
        let operation_span = crate::diagnostics::operation_span(&operation_id.to_string());
        let _operation_guard = operation_span.enter();
        tracing::info!(target: "skills_hub::operation", event_code = "operation_execute_started");
        let _guard = self.coordinator.acquire()?;
        let mut stored = self.store.load(operation_id)?;
        if stored.plan.plan_digest != plan_digest {
            return Err(OperationError::PlanDigestMismatch);
        }
        if stored.journal.state.is_terminal() {
            return terminal_execution(&stored, true);
        }
        if stored.journal.state != OperationState::Planned {
            return Err(OperationError::RecoveryPending(stored.journal.state));
        }

        let domain_result = self
            .domain_preflight
            .preflight(&stored.plan)
            .map_err(|error| OperationError::StalePlan {
                step: None,
                detail: error.to_string(),
            });
        if let Err(error) = domain_result {
            self.finish_no_writes(&mut stored, &error, None, OperationOutcome::FailedNoWrites)?;
            return Err(error);
        }

        if let Err(error) = self.preflight(&stored, cancellation) {
            let outcome = no_write_outcome(&error);
            self.finish_no_writes(&mut stored, &error, None, outcome)?;
            return Err(error);
        }
        self.transition(&mut stored, OperationState::Preflighted, None)?;
        if let Err(error) = self.checkpoint(OperationBoundary::Preflighted) {
            self.finish_no_writes(&mut stored, &error, None, OperationOutcome::FailedNoWrites)?;
            return Err(error);
        }

        let protected_steps: Vec<_> = stored
            .plan
            .content
            .steps
            .iter()
            .filter(|step| step.is_destructive())
            .cloned()
            .collect();
        if let Err(error) = cancellation.check() {
            self.finish_no_writes(
                &mut stored,
                &error,
                None,
                OperationOutcome::CancelledNoWrites,
            )?;
            return Err(error);
        }
        let registration = self
            .snapshots
            .register(&stored.plan, &protected_steps, cancellation)
            .map_err(|source| OperationError::SnapshotFailed(source.to_string()));
        let registration = match registration {
            Ok(registration) => registration,
            Err(snapshot_error) => {
                let (error, outcome) = if cancellation.is_cancelled() {
                    (
                        OperationError::Cancelled,
                        OperationOutcome::CancelledNoWrites,
                    )
                } else {
                    (snapshot_error, OperationOutcome::FailedNoWrites)
                };
                self.finish_no_writes(&mut stored, &error, None, outcome)?;
                return Err(error);
            }
        };
        stored.journal.snapshot_protections =
            match validate_snapshot_registration(&protected_steps, registration) {
                Ok(protections) => protections,
                Err(error) => {
                    self.finish_no_writes(
                        &mut stored,
                        &error,
                        None,
                        OperationOutcome::FailedNoWrites,
                    )?;
                    return Err(error);
                }
            };
        self.transition(&mut stored, OperationState::Snapshotted, None)?;
        if let Err(error) = self.checkpoint(OperationBoundary::SnapshotPublished) {
            self.finish_no_writes(&mut stored, &error, None, OperationOutcome::FailedNoWrites)?;
            return Err(error);
        }

        if let Err(error) = cancellation.check() {
            self.finish_no_writes(
                &mut stored,
                &error,
                None,
                OperationOutcome::CancelledNoWrites,
            )?;
            return Err(error);
        }
        if let Err((error, failed_step)) = self.stage_all(&mut stored, cancellation) {
            self.cleanup_staging(&mut stored);
            let outcome = no_write_outcome(&error);
            self.finish_no_writes(&mut stored, &error, failed_step, outcome)?;
            return Err(error);
        }
        self.transition(&mut stored, OperationState::Staged, None)?;

        if let Err(error) = cancellation.check() {
            self.cleanup_staging(&mut stored);
            self.finish_no_writes(
                &mut stored,
                &error,
                None,
                OperationOutcome::CancelledNoWrites,
            )?;
            return Err(error);
        }

        self.transition(&mut stored, OperationState::Committing, None)?;
        let committed = match self.commit_all(&mut stored) {
            Ok(committed) => committed,
            Err(failure) => {
                if failure.touched.is_empty()
                    || self.active_paths_are_unchanged(&stored, &failure.touched)
                {
                    self.cleanup_staging(&mut stored);
                    self.finish_no_writes(
                        &mut stored,
                        &failure.error,
                        failure.failed_step,
                        OperationOutcome::FailedNoWrites,
                    )?;
                    return Err(failure.error);
                }
                return self.rollback_failure(&mut stored, &failure);
            }
        };
        self.transition(&mut stored, OperationState::Verifying, None)?;
        if let Err(failure) = self.verify_all(&mut stored, committed) {
            return self.rollback_failure(&mut stored, &failure);
        }
        self.transition(&mut stored, OperationState::Committed, None)?;

        self.finalizer
            .publish_manifests(&stored.plan, &stored.journal)
            .map_err(|source| OperationError::FinalizationInterrupted(source.to_string()))?;
        self.checkpoint(OperationBoundary::ManifestsPublished)
            .map_err(|error| OperationError::FinalizationInterrupted(error.to_string()))?;
        self.finalizer
            .finalize_projection(&stored.plan, &stored.journal)
            .map_err(|source| OperationError::FinalizationInterrupted(source.to_string()))?;
        self.checkpoint(OperationBoundary::ProjectionFinalized)
            .map_err(|error| OperationError::FinalizationInterrupted(error.to_string()))?;

        let now = UtcTimestamp::now();
        stored
            .journal
            .transition(OperationState::Finalized, now)
            .map_err(OperationError::Journal)?;
        stored.journal.outcome = Some(OperationOutcome::Succeeded);
        stored.journal.finalized_at = Some(stored.journal.updated_at);
        self.store.write_journal(&stored.journal)?;
        self.events.publish(OperationEvent::Terminal {
            operation_id,
            outcome: OperationOutcome::Succeeded,
        });
        self.events.publish(OperationEvent::Invalidated {
            operation_id,
            resources: vec![
                "library".to_owned(),
                "deployments".to_owned(),
                "activity".to_owned(),
            ],
        });
        let _ = self.checkpoint(OperationBoundary::JournalFinalized);
        self.cleanup_after_success(&mut stored);
        if !stored.journal.cleanup_failures.is_empty() {
            self.store.write_journal(&stored.journal)?;
        }
        tracing::info!(
            target: "skills_hub::operation",
            event_code = "operation_execute_succeeded"
        );
        terminal_execution(&stored, false)
    }

    /// Reconciles one previously authorized, non-terminal Operation from durable evidence.
    /// Terminal Operations are replayed without inspecting or mutating target paths.
    ///
    /// # Errors
    ///
    /// Returns a typed error when recovery rolls back the Operation, requires review, or cannot
    /// durably complete. The resulting journal remains the source of truth in every case.
    pub fn recover(&self, operation_id: OperationId) -> Result<OperationExecution, OperationError> {
        let operation_span = crate::diagnostics::operation_span(&operation_id.to_string());
        let _operation_guard = operation_span.enter();
        tracing::info!(target: "skills_hub::operation", event_code = "operation_recovery_started");
        let _guard = self.coordinator.acquire()?;
        let mut stored = self.store.load(operation_id)?;
        if stored.journal.state.is_terminal() {
            return terminal_execution(&stored, true);
        }
        let decision = classify_startup(&stored, &self.roots)?;
        let recovery_error = OperationError::RecoveryPending(stored.journal.state);
        match decision {
            StartupDecision::AlreadyTerminal => terminal_execution(&stored, true),
            StartupDecision::DiscardStagingAndFailNoWrites => {
                self.cleanup_staging(&mut stored);
                self.finish_no_writes(
                    &mut stored,
                    &recovery_error,
                    None,
                    OperationOutcome::FailedNoWrites,
                )?;
                if !stored.journal.cleanup_failures.is_empty() {
                    self.store.write_journal(&stored.journal)?;
                }
                terminal_execution(&stored, false)
            }
            StartupDecision::RestoreBackupAndFailRolledBack | StartupDecision::ResumeRollback => {
                let touched = recovery_touched_indices(&stored);
                let failure = CommitFailure::new(touched, recovery_error, None);
                self.rollback_failure(&mut stored, &failure)
            }
            StartupDecision::ContinueVerification => {
                if stored.journal.state == OperationState::Committing {
                    self.transition(&mut stored, OperationState::Verifying, None)?;
                }
                let committed = recovery_touched_indices(&stored);
                if let Err(failure) = self.verify_all(&mut stored, committed) {
                    return self.rollback_failure(&mut stored, &failure);
                }
                self.transition(&mut stored, OperationState::Committed, None)?;
                self.finalize_recovered(&mut stored)
            }
            StartupDecision::ContinueFinalization => self.finalize_recovered(&mut stored),
            StartupDecision::CompleteRollback | StartupDecision::MarkFailedRolledBack => {
                if stored.journal.state == OperationState::RollingBack {
                    self.transition(&mut stored, OperationState::RolledBack, None)?;
                }
                self.finish_rolled_back(&mut stored, &recovery_error)?;
                terminal_execution(&stored, false)
            }
            StartupDecision::RecoveryRequired => self.recovery_required(
                &mut stored,
                OperationError::RecoveryRequired(
                    "durable journal and filesystem evidence contradict each other".to_owned(),
                ),
                None,
            ),
        }
    }

    /// Recovers every currently non-terminal Operation in stable Operation-ID order.
    /// This is an explicit startup seam; callers remain responsible for runtime dispatch.
    ///
    /// # Errors
    ///
    /// Returns an error when listing nonterminal Operation IDs fails.
    pub fn recover_pending(&self) -> RecoverPendingResult {
        let ids = self.store.nonterminal_operation_ids()?;
        Ok(ids
            .into_iter()
            .map(|operation_id| (operation_id, self.recover(operation_id)))
            .collect())
    }

    fn finalize_recovered(
        &self,
        stored: &mut StoredOperation,
    ) -> Result<OperationExecution, OperationError> {
        self.finalizer
            .publish_manifests(&stored.plan, &stored.journal)
            .map_err(|source| OperationError::FinalizationInterrupted(source.to_string()))?;
        self.finalizer
            .finalize_projection(&stored.plan, &stored.journal)
            .map_err(|source| OperationError::FinalizationInterrupted(source.to_string()))?;
        stored
            .journal
            .transition(OperationState::Finalized, UtcTimestamp::now())
            .map_err(OperationError::Journal)?;
        stored.journal.outcome = Some(OperationOutcome::Succeeded);
        stored.journal.finalized_at = Some(stored.journal.updated_at);
        self.store.write_journal(&stored.journal)?;
        self.cleanup_after_success(stored);
        if !stored.journal.cleanup_failures.is_empty() {
            self.store.write_journal(&stored.journal)?;
        }
        terminal_execution(stored, false)
    }

    fn finish_rolled_back(
        &self,
        stored: &mut StoredOperation,
        error: &OperationError,
    ) -> Result<(), OperationError> {
        stored.journal.failure = Some(operation_failure(error, None));
        stored
            .journal
            .transition(OperationState::Failed, UtcTimestamp::now())
            .map_err(OperationError::Journal)?;
        stored.journal.outcome = Some(OperationOutcome::FailedRolledBack);
        stored.journal.finalized_at = Some(stored.journal.updated_at);
        self.store.write_journal(&stored.journal)?;
        for index in 0..stored.steps.len() {
            if let Some(path) = stored.steps[index].rollback_path.as_deref() {
                self.cleanup_owned(stored, index, &PathBuf::from(path), ArtifactKind::Rollback);
            }
        }
        self.cleanup_staging(stored);
        if !stored.journal.cleanup_failures.is_empty() {
            self.store.write_journal(&stored.journal)?;
        }
        Ok(())
    }

    fn preflight(
        &self,
        stored: &StoredOperation,
        cancellation: &CancellationToken,
    ) -> Result<(), OperationError> {
        cancellation.check()?;
        stored.plan.verify_digest().map_err(|error| {
            OperationError::InvalidPlan(format!("persisted plan digest is invalid: {error}"))
        })?;
        let now = UtcTimestamp::now();
        if now >= stored.plan.content.expires_at {
            return Err(OperationError::PlanExpired);
        }
        if !stored.plan.content.blockers.is_empty() {
            return Err(OperationError::PlanBlocked(
                stored.plan.content.blockers.len(),
            ));
        }
        for step in &stored.plan.content.steps {
            cancellation.check()?;
            let actual = capture_plan_path(
                &self.roots,
                &step.path,
                &step.before,
                stored.plan.content.bundle_caps,
            )?;
            if !fingerprint_matches(&step.before, &actual) {
                return Err(OperationError::StalePlan {
                    step: Some(step.order),
                    detail: "active path no longer matches the reviewed precondition".to_owned(),
                });
            }
        }
        Ok(())
    }

    fn stage_all(
        &self,
        stored: &mut StoredOperation,
        cancellation: &CancellationToken,
    ) -> Result<(), (OperationError, Option<u32>)> {
        for index in deterministic_indices(&stored.plan) {
            let plan_step = &stored.plan.content.steps[index];
            let evidence = &mut stored.steps[index];
            cancellation
                .check()
                .map_err(|error| (error, Some(plan_step.order)))?;
            if matches!(
                plan_step.action,
                PlanAction::Remove | PlanAction::LeaveUntouched
            ) {
                evidence.stage = PhaseEvidence::not_required();
                evidence.updated_at = UtcTimestamp::now();
                self.store
                    .write_step(evidence)
                    .map_err(|error| (error.into(), Some(plan_step.order)))?;
                continue;
            }

            let final_path = self
                .roots
                .authorize(&plan_step.path)
                .map_err(|error| (error, Some(plan_step.order)))?
                .path()
                .to_path_buf();
            let staging_path = owned_sibling(
                &final_path,
                stored.plan.content.operation_id,
                ArtifactKind::Stage,
            )
            .map_err(|error| (error, Some(plan_step.order)))?;
            ensure_absent(&staging_path).map_err(|error| (error, Some(plan_step.order)))?;
            evidence.stage_path =
                Some(path_text(&staging_path).map_err(|error| (error, Some(plan_step.order)))?);
            let now = UtcTimestamp::now();
            evidence
                .stage
                .record_intent(now)
                .map_err(|error| (error.into(), Some(plan_step.order)))?;
            evidence.updated_at = now;
            self.store
                .write_step(evidence)
                .map_err(|error| (error.into(), Some(plan_step.order)))?;
            self.checkpoint(OperationBoundary::StageIntentPersisted(plan_step.order))
                .map_err(|error| (error, Some(plan_step.order)))?;

            self.stager
                .stage(&stored.plan, plan_step, &staging_path, cancellation)
                .map_err(|source| {
                    let error = if cancellation.is_cancelled() {
                        OperationError::Cancelled
                    } else {
                        OperationError::StageFailed(source.to_string())
                    };
                    (error, Some(plan_step.order))
                })?;
            self.checkpoint(OperationBoundary::StageActionApplied(plan_step.order))
                .map_err(|error| (error, Some(plan_step.order)))?;
            let actual = capture_raw_path(
                &staging_path,
                &plan_step.after,
                stored.plan.content.bundle_caps,
            )
            .map_err(|error| (error, Some(plan_step.order)))?;
            if !fingerprint_matches(&plan_step.after, &actual) {
                return Err((
                    OperationError::StageFailed(
                        "staged result does not match the reviewed postcondition".to_owned(),
                    ),
                    Some(plan_step.order),
                ));
            }
            let now = UtcTimestamp::now();
            evidence
                .stage
                .record_observed(actual, now)
                .map_err(|error| (error.into(), Some(plan_step.order)))?;
            evidence.updated_at = now;
            self.store
                .write_step(evidence)
                .map_err(|error| (error.into(), Some(plan_step.order)))?;
            self.checkpoint(OperationBoundary::StageObserved(plan_step.order))
                .map_err(|error| (error, Some(plan_step.order)))?;
            self.events.publish(OperationEvent::Progress {
                operation_id: stored.plan.content.operation_id,
                state: OperationState::Snapshotted,
                step: Some(plan_step.order),
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn commit_all(&self, stored: &mut StoredOperation) -> Result<Vec<usize>, CommitFailure> {
        let mut touched = Vec::new();
        for index in deterministic_indices(&stored.plan) {
            let plan_step = &stored.plan.content.steps[index];
            if plan_step.action == PlanAction::LeaveUntouched {
                stored.steps[index].commit = PhaseEvidence::not_required();
                stored.steps[index].updated_at = UtcTimestamp::now();
                self.store
                    .write_step(&stored.steps[index])
                    .map_err(|error| {
                        CommitFailure::new(touched.clone(), error.into(), Some(plan_step.order))
                    })?;
                continue;
            }

            self.stager
                .revalidate_before_commit(&stored.plan, plan_step)
                .map_err(|source| {
                    CommitFailure::new(
                        touched.clone(),
                        OperationError::StalePlan {
                            step: Some(plan_step.order),
                            detail: source.to_string(),
                        },
                        Some(plan_step.order),
                    )
                })?;

            let final_path = self.roots.authorize(&plan_step.path).map_err(|error| {
                CommitFailure::new(touched.clone(), error, Some(plan_step.order))
            })?;
            let before = capture_authorized(
                &final_path,
                &plan_step.before,
                stored.plan.content.bundle_caps,
            )
            .map_err(|error| CommitFailure::new(touched.clone(), error, Some(plan_step.order)))?;
            if !fingerprint_matches(&plan_step.before, &before) {
                return Err(CommitFailure::new(
                    touched,
                    OperationError::StalePlan {
                        step: Some(plan_step.order),
                        detail: "active path changed immediately before commit".to_owned(),
                    },
                    Some(plan_step.order),
                ));
            }
            let final_path = final_path.path().to_path_buf();
            if matches!(plan_step.action, PlanAction::Replace | PlanAction::Remove) {
                let backup = owned_sibling(
                    &final_path,
                    stored.plan.content.operation_id,
                    ArtifactKind::Backup,
                )
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
                ensure_absent(&backup).map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
                stored.steps[index].backup_path = Some(path_text(&backup).map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?);
            }
            let now = UtcTimestamp::now();
            stored.steps[index]
                .commit
                .record_intent(now)
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error.into(), Some(plan_step.order))
                })?;
            stored.steps[index].updated_at = now;
            self.store
                .write_step(&stored.steps[index])
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error.into(), Some(plan_step.order))
                })?;
            self.checkpoint(OperationBoundary::CommitIntentPersisted(plan_step.order))
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
            touched.push(index);

            require_plan_path_fingerprint(
                &self.roots,
                &plan_step.path,
                &plan_step.before,
                stored.plan.content.bundle_caps,
                plan_step.order,
                "active path changed after commit intent",
            )
            .map_err(|error| CommitFailure::new(touched.clone(), error, Some(plan_step.order)))?;

            if let Some(backup) = stored.steps[index].backup_path.as_deref() {
                let backup = Path::new(backup);
                rename_sibling(&final_path, backup, plan_step.path.parent_identity()).map_err(
                    |error| CommitFailure::new(touched.clone(), error, Some(plan_step.order)),
                )?;
                require_raw_path_fingerprint(
                    backup,
                    &plan_step.before,
                    stored.plan.content.bundle_caps,
                    plan_step.order,
                    "backup rename did not retain the exact before-version",
                )
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
                self.checkpoint(OperationBoundary::BackupRenamed(plan_step.order))
                    .map_err(|error| {
                        CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                    })?;
                require_raw_path_fingerprint(
                    backup,
                    &plan_step.before,
                    stored.plan.content.bundle_caps,
                    plan_step.order,
                    "backup changed before commit continued",
                )
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
            }

            if matches!(plan_step.action, PlanAction::Create | PlanAction::Replace) {
                ensure_absent(&final_path).map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
                let staging = stored.steps[index].stage_path.as_deref().ok_or_else(|| {
                    CommitFailure::new(
                        touched.clone(),
                        OperationError::InvalidPlan(
                            "create/replace step has no staged path".to_owned(),
                        ),
                        Some(plan_step.order),
                    )
                })?;
                let staged_fingerprint =
                    stored.steps[index].stage.actual.as_ref().ok_or_else(|| {
                        CommitFailure::new(
                            touched.clone(),
                            OperationError::InvalidPlan(
                                "create/replace step has no durable staged fingerprint".to_owned(),
                            ),
                            Some(plan_step.order),
                        )
                    })?;
                require_raw_path_fingerprint(
                    Path::new(staging),
                    staged_fingerprint,
                    stored.plan.content.bundle_caps,
                    plan_step.order,
                    "staged source changed immediately before final rename",
                )
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
                rename_sibling(
                    Path::new(staging),
                    &final_path,
                    plan_step.path.parent_identity(),
                )
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
                self.checkpoint(OperationBoundary::FinalRenamed(plan_step.order))
                    .map_err(|error| {
                        CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                    })?;
            }

            let actual = capture_plan_path(
                &self.roots,
                &plan_step.path,
                &plan_step.after,
                stored.plan.content.bundle_caps,
            )
            .map_err(|error| CommitFailure::new(touched.clone(), error, Some(plan_step.order)))?;
            if !fingerprint_matches(&plan_step.after, &actual) {
                return Err(CommitFailure::new(
                    touched,
                    OperationError::CommitFailed(
                        "actual result does not match the reviewed postcondition".to_owned(),
                    ),
                    Some(plan_step.order),
                ));
            }
            let now = UtcTimestamp::now();
            stored.steps[index]
                .commit
                .record_observed(actual, now)
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error.into(), Some(plan_step.order))
                })?;
            stored.steps[index].updated_at = now;
            self.store
                .write_step(&stored.steps[index])
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error.into(), Some(plan_step.order))
                })?;
            self.checkpoint(OperationBoundary::CommitObserved(plan_step.order))
                .map_err(|error| {
                    CommitFailure::new(touched.clone(), error, Some(plan_step.order))
                })?;
        }
        Ok(touched)
    }

    fn active_paths_are_unchanged(&self, stored: &StoredOperation, touched: &[usize]) -> bool {
        touched.iter().all(|&index| {
            let Some((plan_step, evidence)) = stored
                .plan
                .content
                .steps
                .get(index)
                .zip(stored.steps.get(index))
            else {
                return false;
            };
            let final_is_before = capture_plan_path(
                &self.roots,
                &plan_step.path,
                &plan_step.before,
                stored.plan.content.bundle_caps,
            )
            .is_ok_and(|actual| fingerprint_matches(&plan_step.before, &actual));
            let backup_is_absent = evidence
                .backup_path
                .as_deref()
                .is_none_or(|path| path_is_absent(Path::new(path)));
            final_is_before && backup_is_absent
        })
    }

    fn verify_all(
        &self,
        stored: &mut StoredOperation,
        committed: Vec<usize>,
    ) -> Result<(), CommitFailure> {
        for index in deterministic_indices(&stored.plan) {
            let plan_step = &stored.plan.content.steps[index];
            if plan_step.action == PlanAction::LeaveUntouched {
                stored.steps[index].verify = PhaseEvidence::not_required();
                stored.steps[index].updated_at = UtcTimestamp::now();
                self.store
                    .write_step(&stored.steps[index])
                    .map_err(|error| {
                        CommitFailure::new(committed.clone(), error.into(), Some(plan_step.order))
                    })?;
                continue;
            }
            if stored.steps[index].verify.status == super::PhaseStatus::ObservedComplete {
                continue;
            }
            if stored.steps[index].verify.status == super::PhaseStatus::NotStarted {
                let now = UtcTimestamp::now();
                stored.steps[index]
                    .verify
                    .record_intent(now)
                    .map_err(|error| {
                        CommitFailure::new(committed.clone(), error.into(), Some(plan_step.order))
                    })?;
                stored.steps[index].updated_at = now;
                self.store
                    .write_step(&stored.steps[index])
                    .map_err(|error| {
                        CommitFailure::new(committed.clone(), error.into(), Some(plan_step.order))
                    })?;
                self.checkpoint(OperationBoundary::VerifyIntentPersisted(plan_step.order))
                    .map_err(|error| {
                        CommitFailure::new(committed.clone(), error, Some(plan_step.order))
                    })?;
            }
            let actual = capture_plan_path(
                &self.roots,
                &plan_step.path,
                &plan_step.after,
                stored.plan.content.bundle_caps,
            )
            .map_err(|error| CommitFailure::new(committed.clone(), error, Some(plan_step.order)))?;
            if !fingerprint_matches(&plan_step.after, &actual) {
                return Err(CommitFailure::new(
                    committed,
                    OperationError::VerifyFailed(
                        "final path changed or failed its postcondition".to_owned(),
                    ),
                    Some(plan_step.order),
                ));
            }
            let now = UtcTimestamp::now();
            stored.steps[index]
                .verify
                .record_observed(actual, now)
                .map_err(|error| {
                    CommitFailure::new(committed.clone(), error.into(), Some(plan_step.order))
                })?;
            stored.steps[index].updated_at = now;
            self.store
                .write_step(&stored.steps[index])
                .map_err(|error| {
                    CommitFailure::new(committed.clone(), error.into(), Some(plan_step.order))
                })?;
            self.checkpoint(OperationBoundary::VerifyObserved(plan_step.order))
                .map_err(|error| {
                    CommitFailure::new(committed.clone(), error, Some(plan_step.order))
                })?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn rollback_failure(
        &self,
        stored: &mut StoredOperation,
        failure: &CommitFailure,
    ) -> Result<OperationExecution, OperationError> {
        stored.journal.failure = Some(operation_failure(&failure.error, failure.failed_step));
        if stored.journal.state != OperationState::RollingBack {
            self.transition(stored, OperationState::RollingBack, failure.failed_step)?;
        }
        let mut retained = Vec::new();
        for &index in failure.touched.iter().rev() {
            let plan_step = &stored.plan.content.steps[index];
            let final_path = match self.roots.authorize(&plan_step.path) {
                Ok(path) => path.path().to_path_buf(),
                Err(error) => {
                    return self.recovery_required(stored, error, failure.failed_step);
                }
            };
            let backup = stored.steps[index]
                .backup_path
                .as_deref()
                .map(PathBuf::from);
            let current = match capture_raw_path(
                &final_path,
                &plan_step.after,
                stored.plan.content.bundle_caps,
            ) {
                Ok(actual) => actual,
                Err(error) => return self.recovery_required(stored, error, Some(plan_step.order)),
            };
            let current_is_after = fingerprint_matches(&plan_step.after, &current);
            let current_is_before = capture_raw_path(
                &final_path,
                &plan_step.before,
                stored.plan.content.bundle_caps,
            )
            .is_ok_and(|actual| fingerprint_matches(&plan_step.before, &actual));
            let backup_is_before = backup.as_deref().is_some_and(|path| {
                capture_raw_path(path, &plan_step.before, stored.plan.content.bundle_caps)
                    .is_ok_and(|actual| fingerprint_matches(&plan_step.before, &actual))
            });

            let safe_shape = match plan_step.action {
                PlanAction::Create => current_is_after || current_is_before,
                PlanAction::Replace | PlanAction::Remove => {
                    ((current_is_after || current.expected_kind == EntryKind::Absent)
                        && backup_is_before)
                        || (current_is_before && backup.as_deref().is_none_or(path_is_absent))
                }
                PlanAction::LeaveUntouched => current_is_before,
            };
            if !safe_shape {
                return self.recovery_required(
                    stored,
                    OperationError::RollbackMismatch(plan_step.order),
                    Some(plan_step.order),
                );
            }

            let recorded_rollback = stored.steps[index]
                .rollback_path
                .as_deref()
                .map(PathBuf::from);
            let rollback_path = if recorded_rollback
                .as_deref()
                .is_some_and(std::path::Path::exists)
            {
                recorded_rollback
            } else if current_is_after
                && !current_is_before
                && matches!(plan_step.action, PlanAction::Create | PlanAction::Replace)
            {
                match owned_sibling(
                    &final_path,
                    stored.plan.content.operation_id,
                    ArtifactKind::Rollback,
                ) {
                    Ok(path) => {
                        stored.steps[index].rollback_path = match path_text(&path) {
                            Ok(value) => Some(value),
                            Err(error) => {
                                return self.recovery_required(
                                    stored,
                                    error,
                                    Some(plan_step.order),
                                );
                            }
                        };
                        stored.steps[index].rollback_source = Some(current.clone());
                        Some(path)
                    }
                    Err(error) => {
                        return self.recovery_required(stored, error, Some(plan_step.order));
                    }
                }
            } else {
                None
            };
            if stored.steps[index].rollback.status == super::PhaseStatus::ObservedComplete {
                if let Some(path) = rollback_path {
                    retained.push((index, path));
                }
                continue;
            }
            if stored.steps[index].rollback.status == super::PhaseStatus::NotStarted {
                let now = UtcTimestamp::now();
                if let Err(error) = stored.steps[index].rollback.record_intent(now) {
                    return self.recovery_required(stored, error.into(), Some(plan_step.order));
                }
                stored.steps[index].updated_at = now;
                if let Err(error) = self.store.write_step(&stored.steps[index]) {
                    return self.recovery_required(stored, error.into(), Some(plan_step.order));
                }
                if let Err(error) =
                    self.checkpoint(OperationBoundary::RollbackIntentPersisted(plan_step.order))
                {
                    return self.recovery_required(stored, error, Some(plan_step.order));
                }
            }

            if let Some(rollback_path) = rollback_path {
                let rollback_source = match stored.steps[index].rollback_source.as_ref() {
                    Some(source) => source.clone(),
                    None => {
                        return self.recovery_required(
                            stored,
                            OperationError::RollbackMismatch(plan_step.order),
                            Some(plan_step.order),
                        );
                    }
                };
                if !rollback_path.exists() {
                    if let Err(error) = require_plan_path_fingerprint(
                        &self.roots,
                        &plan_step.path,
                        &rollback_source,
                        stored.plan.content.bundle_caps,
                        plan_step.order,
                        "active path changed after rollback intent",
                    ) {
                        return self.recovery_required(stored, error, Some(plan_step.order));
                    }
                    if let Err(error) = rename_sibling(
                        &final_path,
                        &rollback_path,
                        plan_step.path.parent_identity(),
                    ) {
                        return self.recovery_required(stored, error, Some(plan_step.order));
                    }
                }
                if let Err(error) = require_raw_path_fingerprint(
                    &rollback_path,
                    &rollback_source,
                    stored.plan.content.bundle_caps,
                    plan_step.order,
                    "rollback-aside rename did not retain its exact source",
                ) {
                    return self.recovery_required(stored, error, Some(plan_step.order));
                }
                retained.push((index, rollback_path.clone()));
                if let Err(error) =
                    self.checkpoint(OperationBoundary::RollbackAsideRenamed(plan_step.order))
                {
                    return self.recovery_required(stored, error, Some(plan_step.order));
                }
                if let Err(error) = require_raw_path_fingerprint(
                    &rollback_path,
                    &rollback_source,
                    stored.plan.content.bundle_caps,
                    plan_step.order,
                    "rollback-aside version changed before restore",
                ) {
                    return self.recovery_required(stored, error, Some(plan_step.order));
                }
            }
            if backup_is_before && let Some(backup) = backup.as_deref() {
                if let Err(error) = require_raw_path_fingerprint(
                    backup,
                    &plan_step.before,
                    stored.plan.content.bundle_caps,
                    plan_step.order,
                    "backup changed immediately before restore",
                ) {
                    return self.recovery_required(stored, error, Some(plan_step.order));
                }
                if let Err(error) =
                    rename_sibling(backup, &final_path, plan_step.path.parent_identity())
                {
                    return self.recovery_required(stored, error, Some(plan_step.order));
                }
                if let Err(error) = require_plan_path_fingerprint(
                    &self.roots,
                    &plan_step.path,
                    &plan_step.before,
                    stored.plan.content.bundle_caps,
                    plan_step.order,
                    "backup restore did not produce the exact before-version",
                ) {
                    return self.recovery_required(stored, error, Some(plan_step.order));
                }
            }
            if let Err(error) =
                self.checkpoint(OperationBoundary::RollbackActionApplied(plan_step.order))
            {
                return self.recovery_required(stored, error, Some(plan_step.order));
            }
            let restored = match capture_plan_path(
                &self.roots,
                &plan_step.path,
                &plan_step.before,
                stored.plan.content.bundle_caps,
            ) {
                Ok(actual) => actual,
                Err(error) => return self.recovery_required(stored, error, Some(plan_step.order)),
            };
            if !fingerprint_matches(&plan_step.before, &restored) {
                return self.recovery_required(
                    stored,
                    OperationError::RollbackMismatch(plan_step.order),
                    Some(plan_step.order),
                );
            }
            let now = UtcTimestamp::now();
            if let Err(error) = stored.steps[index].rollback.record_observed(restored, now) {
                return self.recovery_required(stored, error.into(), Some(plan_step.order));
            }
            stored.steps[index].updated_at = now;
            if let Err(error) = self.store.write_step(&stored.steps[index]) {
                return self.recovery_required(stored, error.into(), Some(plan_step.order));
            }
            if let Err(error) =
                self.checkpoint(OperationBoundary::RollbackObserved(plan_step.order))
            {
                return self.recovery_required(stored, error, Some(plan_step.order));
            }
        }

        self.transition(stored, OperationState::RolledBack, failure.failed_step)?;
        let now = UtcTimestamp::now();
        stored
            .journal
            .transition(OperationState::Failed, now)
            .map_err(OperationError::Journal)?;
        stored.journal.outcome = Some(OperationOutcome::FailedRolledBack);
        stored.journal.finalized_at = Some(stored.journal.updated_at);
        self.store.write_journal(&stored.journal)?;
        for (index, path) in retained {
            self.cleanup_owned(stored, index, &path, ArtifactKind::Rollback);
        }
        self.cleanup_staging(stored);
        if !stored.journal.cleanup_failures.is_empty() {
            self.store.write_journal(&stored.journal)?;
        }
        self.events.publish(OperationEvent::Terminal {
            operation_id: stored.plan.content.operation_id,
            outcome: OperationOutcome::FailedRolledBack,
        });
        Err(OperationError::ExecutionFailedRolledBack(
            failure.error.to_string(),
        ))
    }

    fn recovery_required(
        &self,
        stored: &mut StoredOperation,
        error: OperationError,
        failed_step: Option<u32>,
    ) -> Result<OperationExecution, OperationError> {
        let message = error.to_string();
        let now = UtcTimestamp::now();
        if stored.journal.state != OperationState::RecoveryRequired {
            stored
                .journal
                .transition(OperationState::RecoveryRequired, now)
                .map_err(OperationError::Journal)?;
        }
        stored.journal.outcome = Some(OperationOutcome::RecoveryRequired);
        stored.journal.failure = Some(operation_failure(&error, failed_step));
        stored.journal.finalized_at = Some(stored.journal.updated_at);
        self.store.write_journal(&stored.journal)?;
        self.events.publish(OperationEvent::Terminal {
            operation_id: stored.plan.content.operation_id,
            outcome: OperationOutcome::RecoveryRequired,
        });
        drop(error);
        Err(OperationError::RecoveryRequired(message))
    }

    fn finish_no_writes(
        &self,
        stored: &mut StoredOperation,
        error: &OperationError,
        failed_step: Option<u32>,
        outcome: OperationOutcome,
    ) -> Result<(), OperationError> {
        let now = UtcTimestamp::now();
        stored
            .journal
            .transition(OperationState::Failed, now)
            .map_err(OperationError::Journal)?;
        stored.journal.outcome = Some(outcome);
        stored.journal.failure = Some(operation_failure(error, failed_step));
        stored.journal.finalized_at = Some(stored.journal.updated_at);
        self.store.write_journal(&stored.journal)?;
        self.events.publish(OperationEvent::Terminal {
            operation_id: stored.plan.content.operation_id,
            outcome,
        });
        Ok(())
    }

    fn transition(
        &self,
        stored: &mut StoredOperation,
        state: OperationState,
        step: Option<u32>,
    ) -> Result<(), OperationError> {
        stored
            .journal
            .transition(state, UtcTimestamp::now())
            .map_err(OperationError::Journal)?;
        self.store.write_journal(&stored.journal)?;
        self.events.publish(OperationEvent::Progress {
            operation_id: stored.plan.content.operation_id,
            state,
            step,
        });
        Ok(())
    }

    fn checkpoint(&self, boundary: OperationBoundary) -> Result<(), OperationError> {
        self.failpoints
            .check(boundary)
            .map_err(|source| OperationError::InjectedFailure(source.to_string()))
    }

    fn cleanup_staging(&self, stored: &mut StoredOperation) {
        let paths: Vec<_> = stored
            .steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| {
                step.stage_path
                    .as_deref()
                    .map(|path| (index, PathBuf::from(path)))
            })
            .collect();
        for (index, path) in paths {
            self.cleanup_owned(stored, index, &path, ArtifactKind::Stage);
        }
    }

    fn cleanup_after_success(&self, stored: &mut StoredOperation) {
        let backups: Vec<_> = stored
            .steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| {
                step.backup_path
                    .as_deref()
                    .map(|path| (index, PathBuf::from(path)))
            })
            .collect();
        for (index, path) in backups {
            self.cleanup_owned(stored, index, &path, ArtifactKind::Backup);
        }
        self.cleanup_staging(stored);
    }

    fn cleanup_owned(
        &self,
        stored: &mut StoredOperation,
        index: usize,
        path: &Path,
        kind: ArtifactKind,
    ) {
        let path_text = path.to_str();
        let Some((plan_step, evidence)) = stored
            .plan
            .content
            .steps
            .get(index)
            .zip(stored.steps.get(index))
        else {
            stored
                .journal
                .cleanup_failures
                .push(OperationError::CleanupContainment.to_string());
            return;
        };
        let recorded = match kind {
            ArtifactKind::Stage => evidence.stage_path.as_deref(),
            ArtifactKind::Backup => evidence.backup_path.as_deref(),
            ArtifactKind::Rollback => evidence.rollback_path.as_deref(),
        };
        let is_recorded_sibling = recorded == path_text
            && self
                .roots
                .authorize(&plan_step.path)
                .is_ok_and(|final_path| final_path.path().parent() == path.parent());
        if !is_recorded_sibling {
            stored
                .journal
                .cleanup_failures
                .push(OperationError::CleanupContainment.to_string());
            return;
        }
        let expected = match kind {
            ArtifactKind::Stage => evidence.stage.actual.as_ref().unwrap_or(&plan_step.after),
            ArtifactKind::Backup => &plan_step.before,
            ArtifactKind::Rollback => evidence
                .rollback_source
                .as_ref()
                .or(evidence.commit.actual.as_ref())
                .unwrap_or(&plan_step.after),
        };
        if kind == ArtifactKind::Backup
            && !stored
                .journal
                .snapshot_protections
                .iter()
                .any(|protection| {
                    protection.step_order == plan_step.order
                        && protection.before == plan_step.before
                        && !protection.reference.trim().is_empty()
                })
        {
            stored
                .journal
                .cleanup_failures
                .push(OperationError::CleanupContainment.to_string());
            return;
        }
        if let Err(error) = remove_owned_artifact(
            path,
            stored.plan.content.operation_id,
            kind,
            expected,
            stored.plan.content.bundle_caps,
        ) {
            stored.journal.cleanup_failures.push(error.to_string());
        }
    }
}

#[derive(Debug)]
struct CommitFailure {
    touched: Vec<usize>,
    error: OperationError,
    failed_step: Option<u32>,
}

fn recovery_touched_indices(stored: &StoredOperation) -> Vec<usize> {
    deterministic_indices(&stored.plan)
        .into_iter()
        .filter(|&index| stored.plan.content.steps[index].action != PlanAction::LeaveUntouched)
        .collect()
}

impl CommitFailure {
    fn new(touched: Vec<usize>, error: OperationError, failed_step: Option<u32>) -> Self {
        Self {
            touched,
            error,
            failed_step,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactKind {
    Stage,
    Backup,
    Rollback,
}

impl ArtifactKind {
    const fn suffix(self) -> &'static str {
        match self {
            Self::Stage => "stage",
            Self::Backup => "backup",
            Self::Rollback => "rollback",
        }
    }
}

pub(crate) fn capture_plan_path(
    roots: &TargetRoots,
    path: &PlanPath,
    expected: &PathFingerprint,
    caps: BundleCaps,
) -> Result<PathFingerprint, OperationError> {
    let authorized = roots.authorize(path)?;
    capture_authorized(&authorized, expected, caps)
}

fn capture_authorized(
    path: &AuthorizedPath,
    expected: &PathFingerprint,
    caps: BundleCaps,
) -> Result<PathFingerprint, OperationError> {
    let observation = path.inspect().map_err(|error| OperationError::StalePlan {
        step: None,
        detail: error.to_string(),
    })?;
    capture_observation(
        path.path(),
        observation.kind,
        observation.metadata,
        observation.raw_symlink_target,
        expected,
        caps,
    )
}

pub(crate) fn capture_raw_path(
    path: &Path,
    expected: &PathFingerprint,
    caps: BundleCaps,
) -> Result<PathFingerprint, OperationError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return capture_observation(path, EntryKind::Absent, None, None, expected, caps);
        }
        Err(source) => {
            return Err(OperationError::Filesystem {
                context: "inspecting operation path",
                source,
            });
        }
    };
    let fingerprint = MetadataFingerprint::from_metadata(&metadata);
    let raw_symlink_target = if fingerprint.kind == EntryKind::Symlink {
        Some(
            fs::read_link(path).map_err(|source| OperationError::Filesystem {
                context: "reading symbolic-link target",
                source,
            })?,
        )
    } else {
        None
    };
    capture_observation(
        path,
        fingerprint.kind,
        Some(fingerprint),
        raw_symlink_target,
        expected,
        caps,
    )
}

fn capture_observation(
    path: &Path,
    kind: EntryKind,
    metadata: Option<MetadataFingerprint>,
    raw_symlink_target: Option<PathBuf>,
    expected: &PathFingerprint,
    caps: BundleCaps,
) -> Result<PathFingerprint, OperationError> {
    let raw_symlink_target = raw_symlink_target
        .map(|target| {
            target.into_os_string().into_string().map_err(|_| {
                OperationError::FingerprintFailed(
                    "symbolic-link target is not valid UTF-8".to_owned(),
                )
            })
        })
        .transpose()?;
    let bundle_digest = if expected.bundle_digest.is_some() && kind == EntryKind::Directory {
        Some(capture_expected_bundle_digest(path, expected, caps)?)
    } else {
        None
    };
    let resolved_bundle_digest = if expected.resolved_bundle_digest.is_some()
        && kind == EntryKind::Symlink
        && raw_symlink_target == expected.raw_symlink_target
    {
        let target = raw_symlink_target
            .as_deref()
            .ok_or_else(|| OperationError::FingerprintFailed("missing link target".to_owned()))?;
        Some(
            hash_bundle(Path::new(target), caps)
                .map(|hashed| hashed.digest)
                .map_err(|error| OperationError::FingerprintFailed(error.to_string()))?,
        )
    } else {
        None
    };
    Ok(PathFingerprint {
        expected_kind: kind,
        raw_symlink_target,
        metadata,
        bundle_digest,
        bundle_subpath: expected.bundle_subpath.clone(),
        resolved_bundle_digest,
        managed_skill_id: expected.managed_skill_id,
        managed_deployment_id: expected.managed_deployment_id,
        captured_at: UtcTimestamp::now(),
        adapter_id: expected.adapter_id.clone(),
    })
}

fn capture_expected_bundle_digest(
    path: &Path,
    expected: &PathFingerprint,
    caps: BundleCaps,
) -> Result<crate::domain::BundleDigest, OperationError> {
    let bundle = if let Some(relative) = &expected.bundle_subpath {
        let mut current = path.to_path_buf();
        for component in relative.as_str().split('/') {
            let children = fs::read_dir(&current)
                .map_err(|source| OperationError::Filesystem {
                    context: "inspecting fingerprint container",
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| OperationError::Filesystem {
                    context: "reading fingerprint container",
                    source,
                })?;
            let expected_child = children
                .iter()
                .filter(|child| child.file_name() == Path::new(component).as_os_str())
                .count()
                == 1;
            let allowed_manifest_sibling = current == path
                && children.iter().all(|child| {
                    child.file_name() == Path::new(component).as_os_str()
                        || (child.file_name() == "manifest.json"
                            && child.file_type().is_ok_and(|kind| kind.is_file()))
                });
            if !expected_child || (children.len() != 1 && !allowed_manifest_sibling) {
                return Err(OperationError::FingerprintFailed(
                    "fingerprint container does not contain exactly the sealed subpath".to_owned(),
                ));
            }
            current.push(component);
            let metadata =
                fs::symlink_metadata(&current).map_err(|source| OperationError::Filesystem {
                    context: "inspecting fingerprint Bundle subpath",
                    source,
                })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(OperationError::FingerprintFailed(
                    "fingerprint Bundle subpath must be a non-symlink directory".to_owned(),
                ));
            }
        }
        current
    } else {
        path.to_path_buf()
    };
    hash_bundle(&bundle, caps)
        .map(|hashed| hashed.digest)
        .map_err(|error| OperationError::FingerprintFailed(error.to_string()))
}

fn require_plan_path_fingerprint(
    roots: &TargetRoots,
    path: &PlanPath,
    expected: &PathFingerprint,
    caps: BundleCaps,
    step: u32,
    detail: &'static str,
) -> Result<(), OperationError> {
    let actual = capture_plan_path(roots, path, expected, caps)?;
    if fingerprint_matches(expected, &actual) {
        Ok(())
    } else {
        Err(OperationError::StalePlan {
            step: Some(step),
            detail: detail.to_owned(),
        })
    }
}

fn require_raw_path_fingerprint(
    path: &Path,
    expected: &PathFingerprint,
    caps: BundleCaps,
    step: u32,
    detail: &'static str,
) -> Result<(), OperationError> {
    let actual = capture_raw_path(path, expected, caps)?;
    if fingerprint_matches(expected, &actual) {
        Ok(())
    } else {
        Err(OperationError::StalePlan {
            step: Some(step),
            detail: detail.to_owned(),
        })
    }
}

pub(crate) fn fingerprint_matches(expected: &PathFingerprint, actual: &PathFingerprint) -> bool {
    expected.expected_kind == actual.expected_kind
        && expected.raw_symlink_target == actual.raw_symlink_target
        && expected
            .metadata
            .is_none_or(|metadata| actual.metadata == Some(metadata))
        && expected
            .bundle_digest
            .is_none_or(|digest| actual.bundle_digest == Some(digest))
        && expected.bundle_subpath == actual.bundle_subpath
        && expected
            .resolved_bundle_digest
            .is_none_or(|digest| actual.resolved_bundle_digest == Some(digest))
        && expected.managed_skill_id == actual.managed_skill_id
        && expected.managed_deployment_id == actual.managed_deployment_id
        && expected.adapter_id == actual.adapter_id
}

fn validate_snapshot_registration(
    protected_steps: &[PlanStep],
    mut registration: SnapshotRegistration,
) -> Result<Vec<SnapshotProtection>, OperationError> {
    registration
        .protections
        .sort_by_key(|protection| protection.step_order);
    let mut registered = BTreeSet::new();
    for protection in &registration.protections {
        let exact_step = protected_steps
            .iter()
            .any(|step| step.order == protection.step_order && step.before == protection.before);
        if protection.reference.trim().is_empty()
            || !registered.insert(protection.step_order)
            || !exact_step
        {
            return Err(OperationError::SnapshotFailed(
                "Snapshot registration does not attest an exact destructive before-version"
                    .to_owned(),
            ));
        }
    }
    let expected: BTreeSet<_> = protected_steps.iter().map(|step| step.order).collect();
    if registered != expected {
        return Err(OperationError::SnapshotFailed(
            "Snapshot registration does not cover every destructive before-version".to_owned(),
        ));
    }
    Ok(registration.protections)
}

fn deterministic_indices(plan: &OperationPlan) -> Vec<usize> {
    let mut indices: Vec<_> = (0..plan.content.steps.len()).collect();
    indices.sort_by(|left, right| {
        let left = &plan.content.steps[*left];
        let right = &plan.content.steps[*right];
        (left.path.target_id(), left.path.relative(), left.order).cmp(&(
            right.path.target_id(),
            right.path.relative(),
            right.order,
        ))
    });
    indices
}

fn owned_sibling(
    final_path: &Path,
    operation_id: OperationId,
    kind: ArtifactKind,
) -> Result<PathBuf, OperationError> {
    let parent = final_path.parent().ok_or_else(|| {
        OperationError::InvalidPlan("final path has no parent directory".to_owned())
    })?;
    let final_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| OperationError::InvalidPlan("final path name is not UTF-8".to_owned()))?;
    Ok(parent.join(format!(
        ".{final_name}.skills-hub-{operation_id}-{}.{}",
        Uuid::now_v7(),
        kind.suffix()
    )))
}

fn rename_sibling(
    source: &Path,
    destination: &Path,
    expected_parent: PathIdentity,
) -> Result<(), OperationError> {
    let Some(parent) = source.parent() else {
        return Err(OperationError::CleanupContainment);
    };
    if Some(parent) != destination.parent() {
        return Err(OperationError::CleanupContainment);
    }
    let parent_directory = open_revalidated_parent(parent, expected_parent)?;
    let source_name = source
        .file_name()
        .ok_or(OperationError::CleanupContainment)?;
    let destination_name = destination
        .file_name()
        .ok_or(OperationError::CleanupContainment)?;
    renameat_with(
        &parent_directory,
        source_name,
        &parent_directory,
        destination_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(|source| {
        if source == rustix::io::Errno::EXIST {
            OperationError::ArtifactCollision
        } else {
            OperationError::Filesystem {
                context: "atomically renaming operation sibling without replacement",
                source: source.into(),
            }
        }
    })?;
    parent_directory
        .sync_all()
        .map_err(|source| OperationError::Filesystem {
            context: "synchronizing target parent",
            source,
        })?;
    Ok(())
}

fn open_revalidated_parent(parent: &Path, expected: PathIdentity) -> Result<File, OperationError> {
    let metadata = fs::symlink_metadata(parent).map_err(|source| OperationError::Filesystem {
        context: "revalidating final parent before rename",
        source,
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || PathIdentity::from_metadata(&metadata) != expected
    {
        return Err(OperationError::StalePlan {
            step: None,
            detail: "final parent identity changed before rename".to_owned(),
        });
    }
    let directory = File::open(parent).map_err(|source| OperationError::Filesystem {
        context: "opening final parent before rename",
        source,
    })?;
    let opened_identity = directory
        .metadata()
        .map(|metadata| PathIdentity::from_metadata(&metadata))
        .map_err(|source| OperationError::Filesystem {
            context: "revalidating opened final parent before rename",
            source,
        })?;
    if opened_identity != expected {
        return Err(OperationError::StalePlan {
            step: None,
            detail: "opened final parent identity changed before rename".to_owned(),
        });
    }
    Ok(directory)
}

fn ensure_absent(path: &Path) -> Result<(), OperationError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(OperationError::ArtifactCollision),
        Err(source) => Err(OperationError::Filesystem {
            context: "checking operation-owned path",
            source,
        }),
    }
}

fn remove_owned_artifact(
    path: &Path,
    operation_id: OperationId,
    kind: ArtifactKind,
    expected: &PathFingerprint,
    caps: BundleCaps,
) -> Result<(), OperationError> {
    if !artifact_is_owned(path, operation_id, kind) {
        return Err(OperationError::CleanupContainment);
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(OperationError::Filesystem {
                context: "inspecting operation-owned cleanup path",
                source,
            });
        }
    }
    if !has_cleanup_proof(expected) {
        return Err(OperationError::CleanupContainment);
    }
    let actual = capture_raw_path(path, expected, caps)?;
    if !fingerprint_matches(expected, &actual) {
        return Err(OperationError::CleanupContainment);
    }
    let delete_metadata =
        fs::symlink_metadata(path).map_err(|source| OperationError::Filesystem {
            context: "revalidating exact operation-owned cleanup path",
            source,
        })?;
    if actual.metadata != Some(MetadataFingerprint::from_metadata(&delete_metadata)) {
        return Err(OperationError::CleanupContainment);
    }
    if delete_metadata.is_dir() && !delete_metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(|source| OperationError::Filesystem {
        context: "removing exact operation-owned path",
        source,
    })?;
    let parent = path.parent().ok_or(OperationError::CleanupContainment)?;
    sync_directory(parent).map_err(|source| OperationError::Filesystem {
        context: "synchronizing cleanup parent",
        source,
    })?;
    Ok(())
}

fn has_cleanup_proof(expected: &PathFingerprint) -> bool {
    match expected.expected_kind {
        EntryKind::Directory => expected.metadata.is_some() && expected.bundle_digest.is_some(),
        EntryKind::Symlink => expected.metadata.is_some() && expected.raw_symlink_target.is_some(),
        EntryKind::File | EntryKind::Absent | EntryKind::Unsupported => false,
    }
}

pub(crate) fn artifact_is_owned(
    path: &Path,
    operation_id: OperationId,
    kind: ArtifactKind,
) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let marker = format!(".skills-hub-{operation_id}-");
    file_name.starts_with('.')
        && file_name.contains(&marker)
        && file_name.ends_with(&format!(".{}", kind.suffix()))
}

fn path_is_absent(path: &Path) -> bool {
    fs::symlink_metadata(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
}

fn path_text(path: &Path) -> Result<String, OperationError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| OperationError::InvalidPlan("operation path is not UTF-8".to_owned()))
}

fn operation_failure(error: &OperationError, failed_step: Option<u32>) -> OperationFailure {
    let envelope = error.envelope();
    OperationFailure {
        code: envelope.code.to_string(),
        summary: envelope.summary,
        failed_step,
    }
}

fn no_write_outcome(error: &OperationError) -> OperationOutcome {
    if matches!(error, OperationError::Cancelled) {
        OperationOutcome::CancelledNoWrites
    } else {
        OperationOutcome::FailedNoWrites
    }
}

fn terminal_execution(
    stored: &StoredOperation,
    replayed: bool,
) -> Result<OperationExecution, OperationError> {
    let outcome = stored.journal.outcome.ok_or_else(|| {
        OperationError::InvalidPlan("terminal journal has no recorded outcome".to_owned())
    })?;
    Ok(OperationExecution {
        operation_id: stored.plan.content.operation_id,
        state: stored.journal.state,
        outcome,
        failure: stored.journal.failure.clone(),
        cleanup_failures: stored.journal.cleanup_failures.clone(),
        replayed,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationErrorEnvelope {
    pub code: OperationErrorCode,
    pub summary: String,
    pub suggested_action: SuggestedAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationErrorCode {
    MutationBusy,
    CoordinatorUnavailable,
    PlanNotFound,
    PlanDigestMismatch,
    InvalidPlan,
    PlanExpired,
    PlanBlocked,
    StalePlan,
    Cancelled,
    SnapshotFailed,
    StageFailed,
    CommitFailed,
    VerifyFailed,
    FailedRolledBack,
    FinalizationInterrupted,
    RecoveryPending,
    RecoveryRequired,
    JournalInvalid,
    Filesystem,
    CleanupContainment,
    InjectedFailure,
}

impl std::fmt::Display for OperationErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MutationBusy => "mutation_busy",
            Self::CoordinatorUnavailable => "coordinator_unavailable",
            Self::PlanNotFound => "plan_not_found",
            Self::PlanDigestMismatch => "plan_digest_mismatch",
            Self::InvalidPlan => "invalid_plan",
            Self::PlanExpired => "plan_expired",
            Self::PlanBlocked => "plan_blocked",
            Self::StalePlan => "stale_plan",
            Self::Cancelled => "cancelled",
            Self::SnapshotFailed => "snapshot_failed",
            Self::StageFailed => "stage_failed",
            Self::CommitFailed => "commit_failed",
            Self::VerifyFailed => "verify_failed",
            Self::FailedRolledBack => "failed_rolled_back",
            Self::FinalizationInterrupted => "finalization_interrupted",
            Self::RecoveryPending => "recovery_pending",
            Self::RecoveryRequired => "recovery_required",
            Self::JournalInvalid => "journal_invalid",
            Self::Filesystem => "filesystem",
            Self::CleanupContainment => "cleanup_containment",
            Self::InjectedFailure => "injected_failure",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedAction {
    Retry,
    ReviewNewPlan,
    ResolveBlockers,
    WaitForCurrentOperation,
    InspectRecovery,
    RestartToRecover,
    None,
}

#[derive(Debug, Error)]
pub enum OperationError {
    #[error("another mutation Operation is already running for this Vault")]
    MutationBusy,
    #[error("the per-Vault mutation coordinator is unavailable")]
    CoordinatorUnavailable,
    #[error("unknown target ID {0}")]
    UnknownTarget(TargetId),
    #[error("the supplied plan digest does not match the persisted plan")]
    PlanDigestMismatch,
    #[error("Operation Plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("Operation Plan expired before execution")]
    PlanExpired,
    #[error("Operation Plan has {0} unresolved blocker(s)")]
    PlanBlocked(usize),
    #[error("Operation Plan is stale: {detail}")]
    StalePlan { step: Option<u32>, detail: String },
    #[error("Operation was cancelled before active-path commit")]
    Cancelled,
    #[error("Operation snapshot failed: {0}")]
    SnapshotFailed(String),
    #[error("Operation staging failed: {0}")]
    StageFailed(String),
    #[error("Operation commit failed: {0}")]
    CommitFailed(String),
    #[error("Operation verification failed: {0}")]
    VerifyFailed(String),
    #[error("Operation failed and all active paths were rolled back: {0}")]
    ExecutionFailedRolledBack(String),
    #[error("Operation metadata finalization was interrupted: {0}")]
    FinalizationInterrupted(String),
    #[error("Operation is non-terminal in {0:?} and requires startup recovery")]
    RecoveryPending(OperationState),
    #[error("Operation requires reviewed recovery: {0}")]
    RecoveryRequired(String),
    #[error("rollback step {0} found an unexpected active or backup path")]
    RollbackMismatch(u32),
    #[error("operation fingerprint failed: {0}")]
    FingerprintFailed(String),
    #[error("operation-owned sibling path already exists")]
    ArtifactCollision,
    #[error("operation cleanup path failed ownership or containment validation")]
    CleanupContainment,
    #[error("operation filesystem failure while {context}: {source}")]
    Filesystem {
        context: &'static str,
        source: io::Error,
    },
    #[error("operation journal failed: {0}")]
    Journal(#[from] JournalError),
    #[error("injected operation failpoint: {0}")]
    InjectedFailure(String),
}

impl OperationError {
    #[must_use]
    pub fn envelope(&self) -> OperationErrorEnvelope {
        let (code, suggested_action) = match self {
            Self::MutationBusy => (
                OperationErrorCode::MutationBusy,
                SuggestedAction::WaitForCurrentOperation,
            ),
            Self::CoordinatorUnavailable => (
                OperationErrorCode::CoordinatorUnavailable,
                SuggestedAction::RestartToRecover,
            ),
            Self::PlanDigestMismatch => (
                OperationErrorCode::PlanDigestMismatch,
                SuggestedAction::ReviewNewPlan,
            ),
            Self::UnknownTarget(_) | Self::InvalidPlan(_) => (
                OperationErrorCode::InvalidPlan,
                SuggestedAction::ReviewNewPlan,
            ),
            Self::PlanExpired => (
                OperationErrorCode::PlanExpired,
                SuggestedAction::ReviewNewPlan,
            ),
            Self::PlanBlocked(_) => (
                OperationErrorCode::PlanBlocked,
                SuggestedAction::ResolveBlockers,
            ),
            Self::StalePlan { .. } => (
                OperationErrorCode::StalePlan,
                SuggestedAction::ReviewNewPlan,
            ),
            Self::Cancelled => (OperationErrorCode::Cancelled, SuggestedAction::None),
            Self::SnapshotFailed(_) => (OperationErrorCode::SnapshotFailed, SuggestedAction::Retry),
            Self::StageFailed(_) | Self::FingerprintFailed(_) | Self::ArtifactCollision => {
                (OperationErrorCode::StageFailed, SuggestedAction::Retry)
            }
            Self::CommitFailed(_) => (
                OperationErrorCode::CommitFailed,
                SuggestedAction::InspectRecovery,
            ),
            Self::VerifyFailed(_) => (
                OperationErrorCode::VerifyFailed,
                SuggestedAction::InspectRecovery,
            ),
            Self::ExecutionFailedRolledBack(_) => (
                OperationErrorCode::FailedRolledBack,
                SuggestedAction::ReviewNewPlan,
            ),
            Self::FinalizationInterrupted(_) => (
                OperationErrorCode::FinalizationInterrupted,
                SuggestedAction::RestartToRecover,
            ),
            Self::RecoveryPending(_) => (
                OperationErrorCode::RecoveryPending,
                SuggestedAction::RestartToRecover,
            ),
            Self::RecoveryRequired(_) | Self::RollbackMismatch(_) => (
                OperationErrorCode::RecoveryRequired,
                SuggestedAction::InspectRecovery,
            ),
            Self::CleanupContainment => (
                OperationErrorCode::CleanupContainment,
                SuggestedAction::InspectRecovery,
            ),
            Self::Filesystem { .. } => (OperationErrorCode::Filesystem, SuggestedAction::Retry),
            Self::Journal(_) => (
                OperationErrorCode::JournalInvalid,
                SuggestedAction::InspectRecovery,
            ),
            Self::InjectedFailure(_) => (
                OperationErrorCode::InjectedFailure,
                SuggestedAction::InspectRecovery,
            ),
        };
        let summary = match self {
            Self::Journal(_) => "Operation journal evidence is unavailable or invalid.".to_owned(),
            Self::Filesystem { .. } => "A local filesystem operation failed.".to_owned(),
            Self::StalePlan { .. } => {
                "The reviewed Operation Plan no longer matches the filesystem.".to_owned()
            }
            Self::RecoveryRequired(_) | Self::RollbackMismatch(_) => {
                "Automatic compensation stopped to preserve all identifiable versions.".to_owned()
            }
            _ => self.to_string(),
        };
        OperationErrorEnvelope {
            code,
            summary,
            suggested_action,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct OperationHookError {
    message: String,
}

impl OperationHookError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        env,
        fs::{self, File},
        io::Write,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::{Arc, Mutex, atomic::AtomicBool},
        thread,
        time::{Duration, Instant},
    };

    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{
        domain::{AdapterId, BundleRelativePath, DurationMillis},
        filesystem::{BundleStats, hash_bundle},
        operations::{
            OperationKind, OperationPlanContent, PhaseStatus, PlanPath, RecoverySummary,
            StartupDecision, classify_startup,
        },
    };

    struct FixtureStager {
        contents: BTreeMap<u32, String>,
    }

    impl StagingProvider for FixtureStager {
        fn stage(
            &self,
            _plan: &OperationPlan,
            step: &PlanStep,
            staging_path: &Path,
            cancellation: &CancellationToken,
        ) -> Result<(), OperationHookError> {
            cancellation
                .check()
                .map_err(|error| OperationHookError::new(error.to_string()))?;
            let content = self
                .contents
                .get(&step.order)
                .ok_or_else(|| OperationHookError::new("missing staged fixture content"))?;
            write_bundle_exclusive(staging_path, content)
                .map_err(|error| OperationHookError::new(error.to_string()))
        }
    }

    struct TestSnapshots;

    impl SnapshotRegistrar for TestSnapshots {
        fn register(
            &self,
            plan: &OperationPlan,
            protected_steps: &[PlanStep],
            cancellation: &CancellationToken,
        ) -> Result<SnapshotRegistration, OperationHookError> {
            cancellation
                .check()
                .map_err(|error| OperationHookError::new(error.to_string()))?;
            Ok(SnapshotRegistration {
                protections: protected_steps
                    .iter()
                    .map(|step| SnapshotProtection {
                        step_order: step.order,
                        reference: format!("snapshot:operation:{}", plan.content.operation_id),
                        before: step.before.clone(),
                    })
                    .collect(),
            })
        }
    }

    struct EmptySnapshots;

    impl SnapshotRegistrar for EmptySnapshots {
        fn register(
            &self,
            _plan: &OperationPlan,
            _protected_steps: &[PlanStep],
            _cancellation: &CancellationToken,
        ) -> Result<SnapshotRegistration, OperationHookError> {
            Ok(SnapshotRegistration::default())
        }
    }

    struct PartialSnapshots;

    impl SnapshotRegistrar for PartialSnapshots {
        fn register(
            &self,
            plan: &OperationPlan,
            protected_steps: &[PlanStep],
            _cancellation: &CancellationToken,
        ) -> Result<SnapshotRegistration, OperationHookError> {
            Ok(SnapshotRegistration {
                protections: protected_steps
                    .first()
                    .map(|step| SnapshotProtection {
                        step_order: step.order,
                        reference: format!("snapshot:operation:{}", plan.content.operation_id),
                        before: step.before.clone(),
                    })
                    .into_iter()
                    .collect(),
            })
        }
    }

    struct CopyingSnapshots {
        root: PathBuf,
    }

    impl SnapshotRegistrar for CopyingSnapshots {
        fn register(
            &self,
            _plan: &OperationPlan,
            protected_steps: &[PlanStep],
            cancellation: &CancellationToken,
        ) -> Result<SnapshotRegistration, OperationHookError> {
            fs::create_dir_all(&self.root)
                .map_err(|error| OperationHookError::new(error.to_string()))?;
            let mut protections = Vec::with_capacity(protected_steps.len());
            for step in protected_steps {
                cancellation
                    .check()
                    .map_err(|error| OperationHookError::new(error.to_string()))?;
                let snapshot = self.root.join(format!("{:06}", step.order));
                write_bundle(&snapshot, &read_bundle(Path::new(step.path.display_path())))
                    .map_err(|error| OperationHookError::new(error.to_string()))?;
                let digest = hash_bundle(&snapshot, BundleCaps::default())
                    .map_err(|error| OperationHookError::new(error.to_string()))?
                    .digest;
                if step.before.bundle_digest != Some(digest) {
                    return Err(OperationHookError::new(
                        "copied Snapshot does not match the destructive before-version",
                    ));
                }
                protections.push(SnapshotProtection {
                    step_order: step.order,
                    reference: snapshot.to_string_lossy().into_owned(),
                    before: step.before.clone(),
                });
            }
            sync_directory(&self.root)
                .map_err(|error| OperationHookError::new(error.to_string()))?;
            Ok(SnapshotRegistration { protections })
        }
    }

    struct TestFinalizer {
        fail_manifests: bool,
    }

    impl OperationFinalizer for TestFinalizer {
        fn publish_manifests(
            &self,
            _plan: &OperationPlan,
            _journal: &OperationJournal,
        ) -> Result<(), OperationHookError> {
            if self.fail_manifests {
                Err(OperationHookError::new("injected manifest failure"))
            } else {
                Ok(())
            }
        }

        fn finalize_projection(
            &self,
            _plan: &OperationPlan,
            _journal: &OperationJournal,
        ) -> Result<(), OperationHookError> {
            Ok(())
        }
    }

    struct CancelAtCommit(CancellationToken);

    impl OperationEventSink for CancelAtCommit {
        fn publish(&self, event: OperationEvent) {
            if matches!(
                event,
                OperationEvent::Progress {
                    state: OperationState::Committing,
                    ..
                }
            ) {
                self.0.cancel();
            }
        }
    }

    type BoundaryAction = Box<dyn FnOnce() + Send>;

    struct FailOnce {
        boundary: OperationBoundary,
        fired: AtomicBool,
        action: Mutex<Option<BoundaryAction>>,
    }

    impl FailOnce {
        fn at(boundary: OperationBoundary) -> Self {
            Self {
                boundary,
                fired: AtomicBool::new(false),
                action: Mutex::new(None),
            }
        }

        fn with_action(boundary: OperationBoundary, action: BoundaryAction) -> Self {
            Self {
                boundary,
                fired: AtomicBool::new(false),
                action: Mutex::new(Some(action)),
            }
        }
    }

    impl OperationFailpoints for FailOnce {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if boundary == self.boundary && !self.fired.swap(true, Ordering::AcqRel) {
                if let Ok(mut action) = self.action.lock()
                    && let Some(action) = action.take()
                {
                    action();
                }
                Err(OperationHookError::new(format!("boundary {boundary:?}")))
            } else {
                Ok(())
            }
        }
    }

    struct ActionOnce {
        boundary: OperationBoundary,
        action: Mutex<Option<BoundaryAction>>,
    }

    impl ActionOnce {
        fn at(boundary: OperationBoundary, action: BoundaryAction) -> Self {
            Self {
                boundary,
                action: Mutex::new(Some(action)),
            }
        }
    }

    impl OperationFailpoints for ActionOnce {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if boundary == self.boundary
                && let Ok(mut action) = self.action.lock()
                && let Some(action) = action.take()
            {
                action();
            }
            Ok(())
        }
    }

    struct FailAndAct {
        fail_boundary: OperationBoundary,
        action_boundary: OperationBoundary,
        action: Mutex<Option<BoundaryAction>>,
    }

    impl OperationFailpoints for FailAndAct {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if boundary == self.action_boundary
                && let Ok(mut action) = self.action.lock()
                && let Some(action) = action.take()
            {
                action();
            }
            if boundary == self.fail_boundary {
                Err(OperationHookError::new(format!("boundary {boundary:?}")))
            } else {
                Ok(())
            }
        }
    }

    struct FailBoundaries(Vec<OperationBoundary>);

    impl OperationFailpoints for FailBoundaries {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if self.0.contains(&boundary) {
                Err(OperationHookError::new(format!("boundary {boundary:?}")))
            } else {
                Ok(())
            }
        }
    }

    struct ParkAtBoundary {
        boundary: OperationBoundary,
        marker: PathBuf,
    }

    impl OperationFailpoints for ParkAtBoundary {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if boundary == self.boundary {
                let mut marker = File::create(&self.marker)
                    .map_err(|error| OperationHookError::new(error.to_string()))?;
                marker
                    .write_all(b"ready")
                    .and_then(|()| marker.sync_all())
                    .map_err(|error| OperationHookError::new(error.to_string()))?;
                if let Some(parent) = self.marker.parent() {
                    sync_directory(parent)
                        .map_err(|error| OperationHookError::new(error.to_string()))?;
                }
                loop {
                    thread::park_timeout(Duration::from_secs(60));
                }
            }
            Ok(())
        }
    }

    struct FailThenPark {
        fail_boundary: OperationBoundary,
        park: ParkAtBoundary,
    }

    impl OperationFailpoints for FailThenPark {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if boundary == self.fail_boundary {
                Err(OperationHookError::new(format!("boundary {boundary:?}")))
            } else {
                self.park.check(boundary)
            }
        }
    }

    struct Harness {
        temporary: TempDir,
        root: PathBuf,
        target_id: TargetId,
        store: OperationStore,
        roots: TargetRoots,
    }

    impl Harness {
        fn new() -> Self {
            let temporary = tempdir().unwrap();
            let manager = temporary.path().join(".manager");
            let root = temporary.path().join("target");
            fs::create_dir(&manager).unwrap();
            fs::create_dir(&root).unwrap();
            let store = OperationStore::open(&manager).unwrap();
            let target_id = TargetId::generate();
            let mut roots = TargetRoots::new();
            roots.insert(target_id, AuthorizedRoot::open(&root).unwrap());
            Self {
                temporary,
                root,
                target_id,
                store,
                roots,
            }
        }

        fn plan(&self, specs: &[StepSpec<'_>]) -> (OperationPlan, BTreeMap<u32, String>) {
            let created_at = UtcTimestamp::now();
            let adapter = AdapterId::new("operation-test", 1).unwrap();
            let mut steps = Vec::new();
            let mut staged = BTreeMap::new();
            for spec in specs {
                let relative = crate::domain::BundleRelativePath::parse(spec.name).unwrap();
                let authorized = self.roots.roots[&self.target_id]
                    .authorize(&relative)
                    .unwrap();
                let path = PlanPath::from_authorized(self.target_id, &authorized).unwrap();
                let before_template = match spec.before {
                    Some(content) => fingerprint(
                        EntryKind::Directory,
                        Some(bundle_digest(content)),
                        created_at,
                        &adapter,
                    ),
                    None => fingerprint(EntryKind::Absent, None, created_at, &adapter),
                };
                let before =
                    capture_plan_path(&self.roots, &path, &before_template, BundleCaps::default())
                        .unwrap();
                let after = match spec.after {
                    Some(content) => {
                        staged.insert(u32::try_from(steps.len()).unwrap(), content.to_owned());
                        fingerprint(
                            EntryKind::Directory,
                            Some(bundle_digest(content)),
                            created_at,
                            &adapter,
                        )
                    }
                    None => fingerprint(EntryKind::Absent, None, created_at, &adapter),
                };
                steps.push(PlanStep::new(
                    spec.action,
                    path,
                    None,
                    None,
                    before,
                    after,
                    spec.recovery_required,
                ));
            }
            let operation_id = OperationId::generate();
            let expires_at = created_at.checked_add(DurationMillis(300_000)).unwrap();
            let content = OperationPlanContent::new(
                operation_id,
                OperationKind::Deploy,
                created_at,
                expires_at,
                Vec::new(),
                vec![self.target_id],
                Vec::new(),
                Vec::new(),
                BundleCaps::default(),
                BundleStats::default(),
                steps,
                Vec::new(),
                RecoverySummary {
                    snapshot_count: u32::from(specs.iter().any(|step| {
                        matches!(step.action, PlanAction::Replace | PlanAction::Remove)
                    })),
                    estimated_staging_bytes: 1024,
                    estimated_snapshot_bytes: 1024,
                    estimated_rollback_bytes: 1024,
                    spans_filesystems: false,
                },
                Vec::new(),
            );
            (OperationPlan::build(content).unwrap(), staged)
        }

        fn executor(
            &self,
            staged: BTreeMap<u32, String>,
            failpoints: Option<Arc<dyn OperationFailpoints>>,
            fail_finalization: bool,
        ) -> OperationExecutor {
            let executor = OperationExecutor::new(
                self.store.clone(),
                Arc::new(OperationCoordinator::new()),
                self.roots.clone(),
                Arc::new(FixtureStager { contents: staged }),
                Arc::new(TestSnapshots),
                Arc::new(TestFinalizer {
                    fail_manifests: fail_finalization,
                }),
            );
            match failpoints {
                Some(failpoints) => executor.with_failpoints(failpoints),
                None => executor,
            }
        }

        fn persist(&self, plan: &OperationPlan) {
            self.store
                .persist_new_plan(plan, UtcTimestamp::now())
                .unwrap();
        }
    }

    struct StepSpec<'a> {
        name: &'a str,
        action: PlanAction,
        before: Option<&'a str>,
        after: Option<&'a str>,
        recovery_required: bool,
    }

    #[test]
    fn create_replace_remove_finalize_and_replay_without_writes() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("replace"), "old replace").unwrap();
        write_bundle(&harness.root.join("remove"), "old remove").unwrap();
        let (plan, staged) = harness.plan(&[
            StepSpec {
                name: "create",
                action: PlanAction::Create,
                before: None,
                after: Some("new create"),
                recovery_required: false,
            },
            StepSpec {
                name: "replace",
                action: PlanAction::Replace,
                before: Some("old replace"),
                after: Some("new replace"),
                recovery_required: true,
            },
            StepSpec {
                name: "remove",
                action: PlanAction::Remove,
                before: Some("old remove"),
                after: None,
                recovery_required: true,
            },
        ]);
        harness.persist(&plan);
        let executor = harness.executor(staged, None, false);

        let result = executor
            .execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            )
            .unwrap();

        assert_eq!(result.outcome, OperationOutcome::Succeeded);
        assert_eq!(read_bundle(&harness.root.join("create")), "new create");
        assert_eq!(read_bundle(&harness.root.join("replace")), "new replace");
        assert!(!harness.root.join("remove").exists());
        assert_eq!(visible_names(&harness.root), vec!["create", "replace"]);

        let operation = harness.store.operation_directory(plan.content.operation_id);
        assert_eq!(
            visible_names(&operation),
            vec!["journal.json", "plan.json", "steps"]
        );
        assert_eq!(
            visible_names(&operation.join("steps")),
            vec!["000000.json", "000001.json", "000002.json"]
        );
        assert_eq!(
            harness
                .store
                .load(plan.content.operation_id)
                .unwrap()
                .journal
                .snapshot_protections
                .len(),
            2,
            "every destructive before-version has a durable protection attestation"
        );
        let unique_snapshot_references: BTreeSet<_> = harness
            .store
            .load(plan.content.operation_id)
            .unwrap()
            .journal
            .snapshot_protections
            .into_iter()
            .map(|protection| protection.reference)
            .collect();
        assert_eq!(
            unique_snapshot_references.len(),
            1,
            "one Operation-level Snapshot may protect several destructive steps"
        );
        let target_before_replay = tree_bytes(&harness.root);
        let plan_bytes = fs::read(harness.store.plan_path(plan.content.operation_id)).unwrap();
        let replay = executor
            .execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            )
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(tree_bytes(&harness.root), target_before_replay);
        assert_eq!(
            fs::read(harness.store.plan_path(plan.content.operation_id)).unwrap(),
            plan_bytes
        );
    }

    #[test]
    fn operation_store_rejects_extended_noncanonical_plan_bytes() {
        let harness = Harness::new();
        let (plan, _) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        harness.persist(&plan);
        let path = harness.store.plan_path(plan.content.operation_id);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unreviewedField".to_owned(), serde_json::Value::Bool(true));
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        assert!(matches!(
            harness.store.load(plan.content.operation_id),
            Err(JournalError::InvalidPlan(_))
        ));
    }

    #[test]
    fn successful_cleanup_retains_the_protected_before_version() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("skill"), "old").unwrap();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("old"),
            after: Some("new"),
            recovery_required: true,
        }]);
        harness.persist(&plan);
        let snapshots = harness.temporary.path().join("protected-snapshots");
        let executor = OperationExecutor::new(
            harness.store.clone(),
            Arc::new(OperationCoordinator::new()),
            harness.roots.clone(),
            Arc::new(FixtureStager { contents: staged }),
            Arc::new(CopyingSnapshots {
                root: snapshots.clone(),
            }),
            Arc::new(TestFinalizer {
                fail_manifests: false,
            }),
        );

        let result = executor
            .execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            )
            .unwrap();

        assert_eq!(result.outcome, OperationOutcome::Succeeded);
        assert_eq!(read_bundle(&harness.root.join("skill")), "new");
        assert_eq!(visible_names(&harness.root), vec!["skill"]);
        assert_eq!(read_bundle(&snapshots.join("000000")), "old");
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(stored.journal.snapshot_protections.len(), 1);
        assert_eq!(
            stored.journal.snapshot_protections[0].before,
            plan.content.steps[0].before
        );
    }

    #[test]
    fn stage_failure_changes_no_active_path_and_retains_unobserved_evidence() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("a-existing"), "old").unwrap();
        let (plan, staged) = harness.plan(&[
            StepSpec {
                name: "a-existing",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            },
            StepSpec {
                name: "b-create",
                action: PlanAction::Create,
                before: None,
                after: Some("created"),
                recovery_required: false,
            },
        ]);
        harness.persist(&plan);
        let failpoint = Arc::new(FailOnce::at(OperationBoundary::StageActionApplied(1)));
        let executor = harness.executor(staged, Some(failpoint), false);

        assert!(
            executor
                .execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                )
                .is_err()
        );

        assert_eq!(read_bundle(&harness.root.join("a-existing")), "old");
        assert!(!harness.root.join("b-create").exists());
        let names = visible_names(&harness.root);
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"a-existing".to_owned()));
        let retained_stage = names
            .iter()
            .find(|name| {
                Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("stage"))
            })
            .expect("unobserved stage must be retained");
        assert_eq!(read_bundle(&harness.root.join(retained_stage)), "created");
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
        assert_eq!(stored.steps[1].stage.status, PhaseStatus::IntentPersisted);
        assert!(stored.steps[1].stage.actual.is_none());
        assert!(!stored.journal.cleanup_failures.is_empty());
    }

    #[test]
    fn changed_precondition_is_stale_before_staging() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("skill"), "reviewed").unwrap();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("reviewed"),
            after: Some("planned"),
            recovery_required: true,
        }]);
        harness.persist(&plan);
        fs::write(harness.root.join("skill/SKILL.md"), "changed after review").unwrap();
        let executor = harness.executor(staged, None, false);

        let error = executor
            .execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            )
            .unwrap_err();

        assert!(matches!(error, OperationError::StalePlan { .. }));
        assert_eq!(
            read_bundle(&harness.root.join("skill")),
            "changed after review"
        );
        assert_eq!(visible_names(&harness.root), vec!["skill"]);
    }

    #[test]
    fn destructive_operation_requires_a_nonempty_snapshot_registration() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("skill"), "old").unwrap();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("old"),
            after: Some("new"),
            recovery_required: true,
        }]);
        harness.persist(&plan);
        let executor = OperationExecutor::new(
            harness.store.clone(),
            Arc::new(OperationCoordinator::new()),
            harness.roots.clone(),
            Arc::new(FixtureStager { contents: staged }),
            Arc::new(EmptySnapshots),
            Arc::new(TestFinalizer {
                fail_manifests: false,
            }),
        );

        assert!(matches!(
            executor.execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            ),
            Err(OperationError::SnapshotFailed(_))
        ));
        assert_eq!(read_bundle(&harness.root.join("skill")), "old");
        assert_eq!(visible_names(&harness.root), vec!["skill"]);
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
        assert!(stored.journal.snapshot_protections.is_empty());
    }

    #[test]
    fn destructive_operation_rejects_partial_snapshot_coverage() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("a"), "old a").unwrap();
        write_bundle(&harness.root.join("b"), "old b").unwrap();
        let active_before = tree_bytes(&harness.root);
        let (plan, staged) = harness.plan(&[
            StepSpec {
                name: "a",
                action: PlanAction::Replace,
                before: Some("old a"),
                after: Some("new a"),
                recovery_required: true,
            },
            StepSpec {
                name: "b",
                action: PlanAction::Remove,
                before: Some("old b"),
                after: None,
                recovery_required: true,
            },
        ]);
        harness.persist(&plan);
        let executor = OperationExecutor::new(
            harness.store.clone(),
            Arc::new(OperationCoordinator::new()),
            harness.roots.clone(),
            Arc::new(FixtureStager { contents: staged }),
            Arc::new(PartialSnapshots),
            Arc::new(TestFinalizer {
                fail_manifests: false,
            }),
        );

        assert!(matches!(
            executor.execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            ),
            Err(OperationError::SnapshotFailed(_))
        ));
        assert_eq!(tree_bytes(&harness.root), active_before);
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
        assert!(stored.journal.snapshot_protections.is_empty());
    }

    #[test]
    fn every_commit_rename_rechecks_the_frozen_parent_identity() {
        for boundary in [
            OperationBoundary::CommitIntentPersisted(0),
            OperationBoundary::BackupRenamed(0),
        ] {
            let harness = Harness::new();
            let final_parent = harness.root.join("parent");
            let retained_parent = harness.root.join("retained-parent");
            write_bundle(&final_parent.join("skill"), "old").unwrap();
            let (plan, staged) = harness.plan(&[StepSpec {
                name: "parent/skill",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            }]);
            harness.persist(&plan);
            let failpoint = Arc::new(ActionOnce::at(
                boundary,
                Box::new(move || {
                    fs::rename(&final_parent, &retained_parent).unwrap();
                    fs::create_dir(&final_parent).unwrap();
                }),
            ));
            let executor = harness.executor(staged, Some(failpoint), false);

            assert!(matches!(
                executor.execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                ),
                Err(OperationError::RecoveryRequired(_))
            ));
            let retained = harness.root.join("retained-parent");
            let retained_bytes = tree_bytes(&retained)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>();
            assert!(retained_bytes.contains(&b"old".to_vec()));
            assert!(retained_bytes.contains(&b"new".to_vec()));
            assert!(visible_names(&harness.root.join("parent")).is_empty());
            assert_eq!(
                harness
                    .store
                    .load(plan.content.operation_id)
                    .unwrap()
                    .journal
                    .outcome,
                Some(OperationOutcome::RecoveryRequired)
            );
        }
    }

    #[test]
    fn commit_detects_source_replacement_at_each_rename_boundary() {
        for boundary in [
            OperationBoundary::CommitIntentPersisted(0),
            OperationBoundary::BackupRenamed(0),
        ] {
            let harness = Harness::new();
            let final_path = harness.root.join("skill");
            write_bundle(&final_path, "old").unwrap();
            let (plan, staged) = harness.plan(&[StepSpec {
                name: "skill",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            }]);
            harness.persist(&plan);
            let root = harness.root.clone();
            let action: BoundaryAction = if boundary == OperationBoundary::CommitIntentPersisted(0)
            {
                Box::new(move || {
                    fs::rename(root.join("skill"), root.join("retained-old")).unwrap();
                    write_bundle(&root.join("skill"), "foreign").unwrap();
                })
            } else {
                Box::new(move || {
                    let backup = fs::read_dir(&root)
                        .unwrap()
                        .map(|entry| entry.unwrap().path())
                        .find(|path| path.extension().is_some_and(|value| value == "backup"))
                        .unwrap();
                    fs::rename(&backup, root.join("retained-old")).unwrap();
                    write_bundle(&backup, "foreign").unwrap();
                })
            };
            let executor = harness.executor(
                staged,
                Some(Arc::new(ActionOnce::at(boundary, action))),
                false,
            );

            assert!(matches!(
                executor.execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                ),
                Err(OperationError::RecoveryRequired(_))
            ));
            let retained_bytes = tree_bytes(&harness.root)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>();
            for expected in [b"old".as_slice(), b"new".as_slice(), b"foreign".as_slice()] {
                assert!(retained_bytes.contains(&expected.to_vec()));
            }
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.state, OperationState::RecoveryRequired);
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::RecoveryRequired)
            );
        }
    }

    #[test]
    fn every_rollback_rename_rechecks_the_frozen_parent_identity() {
        for action_boundary in [
            OperationBoundary::RollbackIntentPersisted(0),
            OperationBoundary::RollbackAsideRenamed(0),
        ] {
            let harness = Harness::new();
            let final_parent = harness.root.join("parent");
            let retained_parent = harness.root.join("retained-parent");
            write_bundle(&final_parent.join("skill"), "old").unwrap();
            let (plan, staged) = harness.plan(&[StepSpec {
                name: "parent/skill",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            }]);
            harness.persist(&plan);
            let failpoint = Arc::new(FailAndAct {
                fail_boundary: OperationBoundary::VerifyIntentPersisted(0),
                action_boundary,
                action: Mutex::new(Some(Box::new(move || {
                    fs::rename(&final_parent, &retained_parent).unwrap();
                    fs::create_dir(&final_parent).unwrap();
                }))),
            });
            let executor = harness.executor(staged, Some(failpoint), false);

            assert!(matches!(
                executor.execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                ),
                Err(OperationError::RecoveryRequired(_))
            ));
            let retained = harness.root.join("retained-parent");
            let retained_bytes = tree_bytes(&retained)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>();
            assert!(retained_bytes.contains(&b"old".to_vec()));
            assert!(retained_bytes.contains(&b"new".to_vec()));
            assert!(visible_names(&harness.root.join("parent")).is_empty());
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.state, OperationState::RecoveryRequired);
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::RecoveryRequired)
            );
            assert_eq!(
                stored.steps[0].rollback.status,
                PhaseStatus::IntentPersisted
            );
            assert!(stored.steps[0].rollback_source.is_some());
        }
    }

    #[test]
    fn rollback_detects_source_replacement_before_aside_and_restore() {
        for action_boundary in [
            OperationBoundary::RollbackIntentPersisted(0),
            OperationBoundary::RollbackAsideRenamed(0),
        ] {
            let harness = Harness::new();
            write_bundle(&harness.root.join("skill"), "old").unwrap();
            let (plan, staged) = harness.plan(&[StepSpec {
                name: "skill",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            }]);
            harness.persist(&plan);
            let root = harness.root.clone();
            let action: BoundaryAction =
                if action_boundary == OperationBoundary::RollbackIntentPersisted(0) {
                    Box::new(move || {
                        fs::rename(root.join("skill"), root.join("retained-new")).unwrap();
                        write_bundle(&root.join("skill"), "foreign").unwrap();
                    })
                } else {
                    Box::new(move || {
                        let backup = fs::read_dir(&root)
                            .unwrap()
                            .map(|entry| entry.unwrap().path())
                            .find(|path| path.extension().is_some_and(|value| value == "backup"))
                            .unwrap();
                        fs::rename(&backup, root.join("retained-old")).unwrap();
                        write_bundle(&backup, "foreign").unwrap();
                    })
                };
            let failpoint = Arc::new(FailAndAct {
                fail_boundary: OperationBoundary::VerifyIntentPersisted(0),
                action_boundary,
                action: Mutex::new(Some(action)),
            });
            let executor = harness.executor(staged, Some(failpoint), false);

            assert!(matches!(
                executor.execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                ),
                Err(OperationError::RecoveryRequired(_))
            ));
            let retained_bytes = tree_bytes(&harness.root)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>();
            for expected in [b"old".as_slice(), b"new".as_slice(), b"foreign".as_slice()] {
                assert!(retained_bytes.contains(&expected.to_vec()));
            }
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.state, OperationState::RecoveryRequired);
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::RecoveryRequired)
            );
        }
    }

    #[test]
    fn shared_vault_coordinator_rejects_a_second_mutation_without_tree_changes() {
        let harness = Harness::new();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        harness.persist(&plan);
        let coordinator = Arc::new(OperationCoordinator::new());
        let held = coordinator.acquire().unwrap();
        let executor = OperationExecutor::new(
            harness.store.clone(),
            coordinator.clone(),
            harness.roots.clone(),
            Arc::new(FixtureStager { contents: staged }),
            Arc::new(TestSnapshots),
            Arc::new(TestFinalizer {
                fail_manifests: false,
            }),
        );

        assert!(matches!(
            executor.execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            ),
            Err(OperationError::MutationBusy)
        ));
        assert!(visible_names(&harness.root).is_empty());
        assert_eq!(
            harness
                .store
                .load(plan.content.operation_id)
                .unwrap()
                .journal
                .state,
            OperationState::Planned
        );
        drop(held);
    }

    #[test]
    fn cancellation_is_no_write_before_commit_and_ignored_after_commit_starts() {
        let cancelled = Harness::new();
        let (cancelled_plan, cancelled_stage) = cancelled.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        cancelled.persist(&cancelled_plan);
        let token = CancellationToken::default();
        token.cancel();
        let executor = cancelled.executor(cancelled_stage, None, false);
        assert!(matches!(
            executor.execute(
                cancelled_plan.content.operation_id,
                cancelled_plan.plan_digest,
                &token,
            ),
            Err(OperationError::Cancelled)
        ));
        assert_eq!(visible_names(&cancelled.root), Vec::<String>::new());
        assert_eq!(
            cancelled
                .store
                .load(cancelled_plan.content.operation_id)
                .unwrap()
                .journal
                .outcome,
            Some(OperationOutcome::CancelledNoWrites)
        );

        let committed = Harness::new();
        let (committed_plan, committed_stage) = committed.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        committed.persist(&committed_plan);
        let token = CancellationToken::default();
        let executor = committed
            .executor(committed_stage, None, false)
            .with_event_sink(Arc::new(CancelAtCommit(token.clone())));
        let result = executor
            .execute(
                committed_plan.content.operation_id,
                committed_plan.plan_digest,
                &token,
            )
            .unwrap();
        assert!(token.is_cancelled());
        assert_eq!(result.outcome, OperationOutcome::Succeeded);
        assert_eq!(read_bundle(&committed.root.join("skill")), "new");
    }

    #[test]
    fn commit_intent_failure_before_any_rename_is_failed_no_writes() {
        let harness = Harness::new();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        harness.persist(&plan);
        let executor = harness.executor(
            staged,
            Some(Arc::new(FailOnce::at(
                OperationBoundary::CommitIntentPersisted(0),
            ))),
            false,
        );

        assert!(matches!(
            executor.execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            ),
            Err(OperationError::InjectedFailure(_))
        ));
        assert!(visible_names(&harness.root).is_empty());
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Failed);
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
        assert_eq!(stored.steps[0].stage.status, PhaseStatus::ObservedComplete);
        assert!(stored.steps[0].stage.actual.is_some());
        assert_eq!(stored.steps[0].commit.status, PhaseStatus::IntentPersisted);
        assert!(stored.steps[0].commit.actual.is_none());
        assert_eq!(stored.steps[0].rollback.status, PhaseStatus::NotStarted);
    }

    #[test]
    fn pre_rename_source_failure_with_unchanged_active_path_is_failed_no_writes() {
        let harness = Harness::new();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        harness.persist(&plan);
        let root = harness.root.clone();
        let failpoint = Arc::new(ActionOnce::at(
            OperationBoundary::CommitIntentPersisted(0),
            Box::new(move || {
                let stage = fs::read_dir(&root)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.extension().is_some_and(|value| value == "stage"))
                    .unwrap();
                fs::remove_dir_all(stage).unwrap();
            }),
        ));
        let executor = harness.executor(staged, Some(failpoint), false);

        assert!(
            executor
                .execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                )
                .is_err()
        );
        assert!(visible_names(&harness.root).is_empty());
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Failed);
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
        assert_eq!(stored.steps[0].commit.status, PhaseStatus::IntentPersisted);
        assert_eq!(stored.steps[0].rollback.status, PhaseStatus::NotStarted);
    }

    #[test]
    fn commit_and_verify_failures_rollback_in_reverse_order() {
        for boundary in [
            OperationBoundary::FinalRenamed(1),
            OperationBoundary::VerifyObserved(0),
        ] {
            let harness = Harness::new();
            write_bundle(&harness.root.join("a"), "old a").unwrap();
            write_bundle(&harness.root.join("b"), "old b").unwrap();
            let (plan, staged) = harness.plan(&[
                StepSpec {
                    name: "a",
                    action: PlanAction::Replace,
                    before: Some("old a"),
                    after: Some("new a"),
                    recovery_required: true,
                },
                StepSpec {
                    name: "b",
                    action: PlanAction::Replace,
                    before: Some("old b"),
                    after: Some("new b"),
                    recovery_required: true,
                },
            ]);
            harness.persist(&plan);
            let executor = harness.executor(staged, Some(Arc::new(FailOnce::at(boundary))), false);

            let error = executor
                .execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                )
                .unwrap_err();

            assert!(matches!(
                error,
                OperationError::ExecutionFailedRolledBack(_)
            ));
            assert_eq!(read_bundle(&harness.root.join("a")), "old a");
            assert_eq!(read_bundle(&harness.root.join("b")), "old b");
            assert_eq!(visible_names(&harness.root), vec!["a", "b"]);
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::FailedRolledBack)
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn failpoint_matrix_covers_stage_backup_final_and_verify_durability() {
        for boundary in [
            OperationBoundary::StageIntentPersisted(0),
            OperationBoundary::StageActionApplied(0),
            OperationBoundary::StageObserved(0),
        ] {
            let harness = Harness::new();
            let (plan, staged) = harness.plan(&[StepSpec {
                name: "skill",
                action: PlanAction::Create,
                before: None,
                after: Some("new"),
                recovery_required: false,
            }]);
            harness.persist(&plan);
            let executor = harness.executor(staged, Some(Arc::new(FailOnce::at(boundary))), false);

            assert!(
                executor
                    .execute(
                        plan.content.operation_id,
                        plan.plan_digest,
                        &CancellationToken::default(),
                    )
                    .is_err()
            );
            assert!(!harness.root.join("skill").exists());
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.state, OperationState::Failed);
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::FailedNoWrites)
            );
            assert_eq!(
                stored.steps[0].stage.status,
                match boundary {
                    OperationBoundary::StageObserved(0) => PhaseStatus::ObservedComplete,
                    _ => PhaseStatus::IntentPersisted,
                }
            );
            assert_eq!(
                stored.steps[0].stage.actual.is_some(),
                boundary == OperationBoundary::StageObserved(0)
            );
            if boundary == OperationBoundary::StageActionApplied(0) {
                assert_eq!(visible_names(&harness.root).len(), 1);
                assert!(!stored.journal.cleanup_failures.is_empty());
            } else {
                assert!(visible_names(&harness.root).is_empty());
                assert!(stored.journal.cleanup_failures.is_empty());
            }
        }

        for boundary in [
            OperationBoundary::BackupRenamed(0),
            OperationBoundary::FinalRenamed(0),
            OperationBoundary::VerifyIntentPersisted(0),
            OperationBoundary::VerifyObserved(0),
        ] {
            let harness = Harness::new();
            write_bundle(&harness.root.join("skill"), "old").unwrap();
            let (plan, staged) = harness.plan(&[StepSpec {
                name: "skill",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            }]);
            harness.persist(&plan);
            let executor = harness.executor(staged, Some(Arc::new(FailOnce::at(boundary))), false);

            assert!(matches!(
                executor.execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                ),
                Err(OperationError::ExecutionFailedRolledBack(_))
            ));
            assert_eq!(read_bundle(&harness.root.join("skill")), "old");
            assert_eq!(visible_names(&harness.root), vec!["skill"]);
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.state, OperationState::Failed);
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::FailedRolledBack)
            );
            assert!(stored.journal.cleanup_failures.is_empty());
            assert_eq!(stored.steps[0].stage.status, PhaseStatus::ObservedComplete);
            assert_eq!(
                stored.steps[0].commit.status,
                match boundary {
                    OperationBoundary::BackupRenamed(0) | OperationBoundary::FinalRenamed(0) =>
                        PhaseStatus::IntentPersisted,
                    _ => PhaseStatus::ObservedComplete,
                }
            );
            assert_eq!(
                stored.steps[0].verify.status,
                match boundary {
                    OperationBoundary::VerifyIntentPersisted(0) => PhaseStatus::IntentPersisted,
                    OperationBoundary::VerifyObserved(0) => PhaseStatus::ObservedComplete,
                    _ => PhaseStatus::NotStarted,
                }
            );
            assert_eq!(
                stored.steps[0].rollback.status,
                PhaseStatus::ObservedComplete
            );
            assert!(stored.steps[0].rollback.actual.is_some());
        }
    }

    #[test]
    fn failpoint_matrix_covers_both_critical_finalization_boundaries() {
        for boundary in [
            OperationBoundary::ManifestsPublished,
            OperationBoundary::ProjectionFinalized,
        ] {
            let harness = Harness::new();
            write_bundle(&harness.root.join("skill"), "old").unwrap();
            let (plan, staged) = harness.plan(&[StepSpec {
                name: "skill",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            }]);
            harness.persist(&plan);
            let executor = harness.executor(staged, Some(Arc::new(FailOnce::at(boundary))), false);

            assert!(matches!(
                executor.execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                ),
                Err(OperationError::FinalizationInterrupted(_))
            ));
            assert_eq!(read_bundle(&harness.root.join("skill")), "new");
            let retained_bytes = tree_bytes(&harness.root)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>();
            assert!(retained_bytes.contains(&b"old".to_vec()));
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.state, OperationState::Committed);
            assert_eq!(stored.journal.outcome, None);
            assert_eq!(stored.steps[0].commit.status, PhaseStatus::ObservedComplete);
            assert!(stored.steps[0].commit.actual.is_some());
            assert_eq!(stored.steps[0].verify.status, PhaseStatus::ObservedComplete);
            assert!(stored.steps[0].verify.actual.is_some());
            assert!(
                stored.steps[0]
                    .backup_path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).exists())
            );
            assert_eq!(stored.journal.snapshot_protections.len(), 1);
            assert_eq!(
                classify_startup(&stored, &harness.roots).unwrap(),
                StartupDecision::ContinueFinalization
            );
        }
    }

    #[test]
    fn failpoint_matrix_covers_each_rollback_durability_boundary() {
        for rollback_boundary in [
            OperationBoundary::RollbackIntentPersisted(1),
            OperationBoundary::RollbackAsideRenamed(1),
            OperationBoundary::RollbackActionApplied(1),
            OperationBoundary::RollbackObserved(1),
        ] {
            let harness = Harness::new();
            write_bundle(&harness.root.join("a"), "old a").unwrap();
            write_bundle(&harness.root.join("b"), "old b").unwrap();
            let (plan, staged) = harness.plan(&[
                StepSpec {
                    name: "a",
                    action: PlanAction::Replace,
                    before: Some("old a"),
                    after: Some("new a"),
                    recovery_required: true,
                },
                StepSpec {
                    name: "b",
                    action: PlanAction::Replace,
                    before: Some("old b"),
                    after: Some("new b"),
                    recovery_required: true,
                },
            ]);
            harness.persist(&plan);
            let failpoints = Arc::new(FailBoundaries(vec![
                OperationBoundary::FinalRenamed(1),
                rollback_boundary,
            ]));
            let executor = harness.executor(staged, Some(failpoints), false);

            assert!(matches!(
                executor.execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                ),
                Err(OperationError::RecoveryRequired(_))
            ));
            let retained_bytes = tree_bytes(&harness.root)
                .into_iter()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>();
            for expected in [b"old a", b"new a", b"old b", b"new b"] {
                assert!(retained_bytes.contains(&expected.to_vec()));
            }
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.state, OperationState::RecoveryRequired);
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::RecoveryRequired)
            );
            assert_eq!(
                stored.steps[1].rollback.status,
                if rollback_boundary == OperationBoundary::RollbackObserved(1) {
                    PhaseStatus::ObservedComplete
                } else {
                    PhaseStatus::IntentPersisted
                }
            );
            assert!(stored.steps[1].rollback_path.is_some());
            assert!(stored.steps[1].rollback_source.is_some());
        }
    }

    #[test]
    fn rollback_mismatch_preserves_before_new_and_interfering_versions() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("a"), "old a").unwrap();
        write_bundle(&harness.root.join("b"), "old b").unwrap();
        let (plan, staged) = harness.plan(&[
            StepSpec {
                name: "a",
                action: PlanAction::Replace,
                before: Some("old a"),
                after: Some("new a"),
                recovery_required: true,
            },
            StepSpec {
                name: "b",
                action: PlanAction::Replace,
                before: Some("old b"),
                after: Some("new b"),
                recovery_required: true,
            },
        ]);
        harness.persist(&plan);
        let interfered = harness.root.join("a/SKILL.md");
        let failpoint = Arc::new(FailOnce::with_action(
            OperationBoundary::VerifyIntentPersisted(0),
            Box::new(move || {
                fs::write(interfered, "same-user interference").unwrap();
            }),
        ));
        let executor = harness.executor(staged, Some(failpoint), false);

        let error = executor
            .execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            )
            .unwrap_err();

        assert!(matches!(error, OperationError::RecoveryRequired(_)));
        assert_eq!(
            read_bundle(&harness.root.join("a")),
            "same-user interference"
        );
        let hidden = visible_names(&harness.root)
            .into_iter()
            .filter(|name| name.starts_with('.'))
            .collect::<Vec<_>>();
        assert!(hidden.len() >= 2, "retained evidence: {hidden:?}");
        let retained_bytes = hidden
            .iter()
            .filter_map(|name| {
                let path = harness.root.join(name);
                path.is_dir().then(|| read_bundle(&path))
            })
            .collect::<Vec<_>>();
        assert!(retained_bytes.contains(&"old a".to_owned()));
        assert!(retained_bytes.contains(&"new b".to_owned()));
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::RecoveryRequired)
        );
    }

    #[test]
    fn successful_cleanup_failure_is_retained_journaled_and_returned() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("skill"), "old").unwrap();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("old"),
            after: Some("new"),
            recovery_required: true,
        }]);
        harness.persist(&plan);
        let root = harness.root.clone();
        let failpoint = Arc::new(ActionOnce::at(
            OperationBoundary::JournalFinalized,
            Box::new(move || {
                let backup = fs::read_dir(&root)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.extension().is_some_and(|value| value == "backup"))
                    .unwrap();
                fs::rename(&backup, root.join("retained-old")).unwrap();
                write_bundle(&backup, "foreign").unwrap();
            }),
        ));
        let executor = harness.executor(staged, Some(failpoint), false);

        let execution = executor
            .execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            )
            .unwrap();

        assert_eq!(execution.outcome, OperationOutcome::Succeeded);
        assert!(!execution.cleanup_failures.is_empty());
        let retained_bytes = tree_bytes(&harness.root)
            .into_iter()
            .map(|(_, bytes)| bytes)
            .collect::<Vec<_>>();
        for expected in [b"old".as_slice(), b"new".as_slice(), b"foreign".as_slice()] {
            assert!(retained_bytes.contains(&expected.to_vec()));
        }
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Finalized);
        assert_eq!(stored.journal.cleanup_failures, execution.cleanup_failures);
    }

    #[test]
    fn cleanup_rejects_unowned_target_content() {
        let directory = tempdir().unwrap();
        let ordinary = directory.path().join("ordinary");
        write_bundle(&ordinary, "keep me").unwrap();
        let expected = fingerprint(
            EntryKind::Directory,
            Some(bundle_digest("keep me")),
            UtcTimestamp::now(),
            &AdapterId::new("cleanup-test", 1).unwrap(),
        );

        let error = remove_owned_artifact(
            &ordinary,
            OperationId::generate(),
            ArtifactKind::Stage,
            &expected,
            BundleCaps::default(),
        )
        .unwrap_err();

        assert!(matches!(error, OperationError::CleanupContainment));
        assert_eq!(read_bundle(&ordinary), "keep me");
    }

    #[test]
    fn cleanup_rejects_owned_marker_without_durable_identity_and_content_proof() {
        let directory = tempdir().unwrap();
        let operation_id = OperationId::generate();
        let final_path = directory.path().join("skill");
        let staged = owned_sibling(&final_path, operation_id, ArtifactKind::Stage).unwrap();
        write_bundle(&staged, "keep me").unwrap();
        let kind_only = fingerprint(
            EntryKind::Directory,
            None,
            UtcTimestamp::now(),
            &AdapterId::new("cleanup-test", 1).unwrap(),
        );

        assert!(matches!(
            remove_owned_artifact(
                &staged,
                operation_id,
                ArtifactKind::Stage,
                &kind_only,
                BundleCaps::default(),
            ),
            Err(OperationError::CleanupContainment)
        ));
        assert_eq!(read_bundle(&staged), "keep me");
    }

    #[test]
    fn cleanup_retains_owned_content_changed_after_durable_observation() {
        let directory = tempdir().unwrap();
        let operation_id = OperationId::generate();
        let final_path = directory.path().join("skill");
        let staged = owned_sibling(&final_path, operation_id, ArtifactKind::Stage).unwrap();
        write_bundle(&staged, "reviewed").unwrap();
        let expected_template = fingerprint(
            EntryKind::Directory,
            Some(bundle_digest("reviewed")),
            UtcTimestamp::now(),
            &AdapterId::new("cleanup-test", 1).unwrap(),
        );
        let observed =
            capture_raw_path(&staged, &expected_template, BundleCaps::default()).unwrap();
        fs::write(staged.join("SKILL.md"), "changed").unwrap();

        assert!(matches!(
            remove_owned_artifact(
                &staged,
                operation_id,
                ArtifactKind::Stage,
                &observed,
                BundleCaps::default(),
            ),
            Err(OperationError::CleanupContainment)
        ));
        assert_eq!(read_bundle(&staged), "changed");
    }

    #[test]
    fn operation_errors_have_stable_codes_actions_and_redacted_filesystem_summaries() {
        let stale = OperationError::StalePlan {
            step: Some(3),
            detail: "changed".to_owned(),
        }
        .envelope();
        assert_eq!(stale.code, OperationErrorCode::StalePlan);
        assert_eq!(stale.suggested_action, SuggestedAction::ReviewNewPlan);

        let recovery = OperationError::RollbackMismatch(2).envelope();
        assert_eq!(recovery.code, OperationErrorCode::RecoveryRequired);
        assert_eq!(recovery.suggested_action, SuggestedAction::InspectRecovery);

        let filesystem = OperationError::Filesystem {
            context: "reading /private/secret/path",
            source: io::Error::new(io::ErrorKind::PermissionDenied, "private detail"),
        }
        .envelope();
        assert_eq!(filesystem.code, OperationErrorCode::Filesystem);
        assert_eq!(filesystem.suggested_action, SuggestedAction::Retry);
        assert_eq!(filesystem.summary, "A local filesystem operation failed.");
        assert!(!filesystem.summary.contains("secret"));
        assert!(!filesystem.summary.contains("private"));
    }

    #[test]
    fn sibling_rename_never_replaces_a_destination_that_appeared() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        write_bundle(&source, "planned").unwrap();
        write_bundle(&destination, "foreign").unwrap();
        let parent_identity =
            PathIdentity::from_metadata(&fs::symlink_metadata(directory.path()).unwrap());

        assert!(matches!(
            rename_sibling(&source, &destination, parent_identity),
            Err(OperationError::ArtifactCollision)
        ));
        assert_eq!(read_bundle(&source), "planned");
        assert_eq!(read_bundle(&destination), "foreign");
    }

    #[test]
    fn cleanup_retains_a_recorded_marker_when_durable_identity_no_longer_matches() {
        let harness = Harness::new();
        let (plan, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        harness.persist(&plan);
        let root = harness.root.clone();
        let failpoint = Arc::new(FailOnce::with_action(
            OperationBoundary::StageObserved(0),
            Box::new(move || {
                let staged_path = fs::read_dir(&root)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("stage"))
                    })
                    .unwrap();
                fs::remove_dir_all(&staged_path).unwrap();
                write_bundle(&staged_path, "new").unwrap();
            }),
        ));
        let executor = harness.executor(staged, Some(failpoint), false);

        assert!(
            executor
                .execute(
                    plan.content.operation_id,
                    plan.plan_digest,
                    &CancellationToken::default(),
                )
                .is_err()
        );
        assert!(!harness.root.join("skill").exists());
        let stage = visible_names(&harness.root)
            .into_iter()
            .find(|name| {
                Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("stage"))
            })
            .expect("forged marker must be retained");
        assert_eq!(read_bundle(&harness.root.join(stage)), "new");
        let stored = harness.store.load(plan.content.operation_id).unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
        assert!(!stored.journal.cleanup_failures.is_empty());
    }

    #[test]
    #[ignore = "invoked only by child_process_kill_reopens_and_recovers_idempotently"]
    fn child_process_crash_helper() {
        let (Ok(manager), Ok(root), Ok(marker), Ok(boundary)) = (
            env::var("SKILLS_HUB_M0_005_CRASH_MANAGER"),
            env::var("SKILLS_HUB_M0_005_CRASH_ROOT"),
            env::var("SKILLS_HUB_M0_005_CRASH_MARKER"),
            env::var("SKILLS_HUB_M0_005_CRASH_BOUNDARY"),
        ) else {
            return;
        };
        let store = OperationStore::open(Path::new(&manager)).unwrap();
        let operation_id = store.nonterminal_operation_ids().unwrap()[0];
        let stored = store.load(operation_id).unwrap();
        let target_id = stored.plan.content.steps[0].path.target_id();
        let mut roots = TargetRoots::new();
        roots.insert(target_id, AuthorizedRoot::open(&root).unwrap());
        let marker = PathBuf::from(marker);
        let failpoints: Arc<dyn OperationFailpoints> = match boundary.as_str() {
            "commit_intent" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::CommitIntentPersisted(0),
                marker,
            }),
            "backup" | "backup_contradiction" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::BackupRenamed(0),
                marker,
            }),
            "final" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::FinalRenamed(0),
                marker,
            }),
            "commit_observed" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::CommitObserved(0),
                marker,
            }),
            "verify_intent" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::VerifyIntentPersisted(0),
                marker,
            }),
            "verify_observed" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::VerifyObserved(0),
                marker,
            }),
            "manifests_published" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::ManifestsPublished,
                marker,
            }),
            "projection_finalized" => Arc::new(ParkAtBoundary {
                boundary: OperationBoundary::ProjectionFinalized,
                marker,
            }),
            "rollback_intent" | "rollback_aside" | "rollback_action" | "rollback_observed" => {
                Arc::new(FailThenPark {
                    fail_boundary: OperationBoundary::VerifyIntentPersisted(0),
                    park: ParkAtBoundary {
                        boundary: match boundary.as_str() {
                            "rollback_intent" => OperationBoundary::RollbackIntentPersisted(0),
                            "rollback_aside" => OperationBoundary::RollbackAsideRenamed(0),
                            "rollback_action" => OperationBoundary::RollbackActionApplied(0),
                            "rollback_observed" => OperationBoundary::RollbackObserved(0),
                            _ => unreachable!(),
                        },
                        marker,
                    },
                })
            }
            value => panic!("unsupported crash boundary {value}"),
        };
        let executor = OperationExecutor::new(
            store,
            Arc::new(OperationCoordinator::new()),
            roots,
            Arc::new(FixtureStager {
                contents: BTreeMap::from([(0, "new".to_owned())]),
            }),
            Arc::new(TestSnapshots),
            Arc::new(TestFinalizer {
                fail_manifests: false,
            }),
        )
        .with_failpoints(failpoints);

        let _ = executor.execute(
            operation_id,
            stored.plan.plan_digest,
            &CancellationToken::default(),
        );
        panic!("crash helper unexpectedly returned");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn child_process_kill_reopens_and_recovers_idempotently() {
        for (boundary, expected_outcome, expected_content) in [
            ("commit_intent", OperationOutcome::FailedNoWrites, "old"),
            ("backup", OperationOutcome::FailedRolledBack, "old"),
            (
                "backup_contradiction",
                OperationOutcome::RecoveryRequired,
                "foreign",
            ),
            ("final", OperationOutcome::Succeeded, "new"),
            ("commit_observed", OperationOutcome::Succeeded, "new"),
            ("verify_intent", OperationOutcome::Succeeded, "new"),
            ("verify_observed", OperationOutcome::Succeeded, "new"),
            ("rollback_intent", OperationOutcome::FailedRolledBack, "old"),
            ("rollback_aside", OperationOutcome::FailedRolledBack, "old"),
            ("rollback_action", OperationOutcome::FailedRolledBack, "old"),
            (
                "rollback_observed",
                OperationOutcome::FailedRolledBack,
                "old",
            ),
            ("manifests_published", OperationOutcome::Succeeded, "new"),
            ("projection_finalized", OperationOutcome::Succeeded, "new"),
        ] {
            let harness = Harness::new();
            write_bundle(&harness.root.join("skill"), "old").unwrap();
            let (plan, _) = harness.plan(&[StepSpec {
                name: "skill",
                action: PlanAction::Replace,
                before: Some("old"),
                after: Some("new"),
                recovery_required: true,
            }]);
            harness.persist(&plan);
            let marker = harness.temporary.path().join("crash-ready");
            let manager = harness.store.operations_root().parent().unwrap();
            let mut child = Command::new(env::current_exe().unwrap())
                .arg("--ignored")
                .arg("--exact")
                .arg("operations::executor::tests::child_process_crash_helper")
                .arg("--test-threads=1")
                .env("SKILLS_HUB_M0_005_CRASH_MANAGER", manager)
                .env("SKILLS_HUB_M0_005_CRASH_ROOT", &harness.root)
                .env("SKILLS_HUB_M0_005_CRASH_MARKER", &marker)
                .env("SKILLS_HUB_M0_005_CRASH_BOUNDARY", boundary)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(15);
            while !marker.exists() {
                assert!(
                    Instant::now() < deadline,
                    "child did not reach crash marker"
                );
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "child exited before crash marker"
                );
                thread::sleep(Duration::from_millis(10));
            }
            child.kill().unwrap();
            assert!(!child.wait().unwrap().success());
            if boundary == "backup_contradiction" {
                write_bundle(&harness.root.join("skill"), "foreign").unwrap();
            }

            let reopened = OperationStore::open(manager).unwrap();
            let recovery = OperationExecutor::new(
                reopened,
                Arc::new(OperationCoordinator::new()),
                harness.roots.clone(),
                Arc::new(FixtureStager {
                    contents: BTreeMap::new(),
                }),
                Arc::new(TestSnapshots),
                Arc::new(TestFinalizer {
                    fail_manifests: false,
                }),
            );
            let first = recovery.recover(plan.content.operation_id);
            if expected_outcome == OperationOutcome::RecoveryRequired {
                assert!(
                    matches!(first, Err(OperationError::RecoveryRequired(_))),
                    "{first:?}"
                );
            } else if expected_outcome == OperationOutcome::FailedRolledBack {
                assert!(
                    matches!(
                        first,
                        Ok(OperationExecution {
                            outcome: OperationOutcome::FailedRolledBack,
                            replayed: false,
                            ..
                        }) | Err(OperationError::ExecutionFailedRolledBack(_))
                    ),
                    "{boundary}: {first:?}"
                );
            } else {
                let execution = first.unwrap();
                assert_eq!(execution.outcome, expected_outcome, "{boundary}");
                assert!(!execution.replayed, "{boundary}");
            }
            assert_eq!(
                read_bundle(&harness.root.join("skill")),
                expected_content,
                "{boundary}"
            );
            if expected_outcome == OperationOutcome::RecoveryRequired {
                let retained = tree_bytes(&harness.root)
                    .into_iter()
                    .map(|(_, bytes)| bytes)
                    .collect::<Vec<_>>();
                for version in [b"old".as_slice(), b"new".as_slice(), b"foreign".as_slice()] {
                    assert!(retained.contains(&version.to_vec()), "{boundary}");
                }
            } else {
                assert_eq!(visible_names(&harness.root), vec!["skill"], "{boundary}");
            }
            let stored = harness.store.load(plan.content.operation_id).unwrap();
            assert_eq!(stored.journal.outcome, Some(expected_outcome), "{boundary}");
            let target_after_recovery = tree_bytes(&harness.root);
            let journal_after_recovery = serde_json::to_vec(&stored.journal).unwrap();

            let replay = recovery.recover(plan.content.operation_id).unwrap();
            assert!(replay.replayed, "{boundary}");
            assert_eq!(replay.outcome, expected_outcome, "{boundary}");
            assert_eq!(
                tree_bytes(&harness.root),
                target_after_recovery,
                "{boundary}"
            );
            assert_eq!(
                serde_json::to_vec(
                    &harness
                        .store
                        .load(plan.content.operation_id)
                        .unwrap()
                        .journal
                )
                .unwrap(),
                journal_after_recovery,
                "{boundary}"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn startup_classifier_handles_clean_committed_and_contradictory_real_trees() {
        let harness = Harness::new();
        write_bundle(&harness.root.join("skill"), "old").unwrap();
        let (planned, staged) = harness.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("old"),
            after: Some("new"),
            recovery_required: true,
        }]);
        harness.persist(&planned);
        let stored = harness.store.load(planned.content.operation_id).unwrap();
        fs::write(
            harness.store.operations_root().join(".DS_Store"),
            b"ignored",
        )
        .unwrap();
        assert_eq!(
            harness.store.nonterminal_operation_ids().unwrap(),
            vec![planned.content.operation_id]
        );
        assert_eq!(
            classify_startup(&stored, &harness.roots).unwrap(),
            StartupDecision::DiscardStagingAndFailNoWrites
        );

        let executor = harness.executor(staged, None, true);
        assert!(matches!(
            executor.execute(
                planned.content.operation_id,
                planned.plan_digest,
                &CancellationToken::default(),
            ),
            Err(OperationError::FinalizationInterrupted(_))
        ));
        let committed = harness.store.load(planned.content.operation_id).unwrap();
        assert_eq!(committed.journal.state, OperationState::Committed);
        assert_eq!(
            classify_startup(&committed, &harness.roots).unwrap(),
            StartupDecision::ContinueFinalization
        );

        let second = Harness::new();
        write_bundle(&second.root.join("skill"), "old").unwrap();
        let (contradictory_plan, _) = second.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("old"),
            after: Some("new"),
            recovery_required: true,
        }]);
        second.persist(&contradictory_plan);
        let mut contradictory = second
            .store
            .load(contradictory_plan.content.operation_id)
            .unwrap();
        contradictory.steps[0].stage_path = Some(
            second
                .root
                .join("ordinary-user-directory")
                .to_string_lossy()
                .into_owned(),
        );
        assert_eq!(
            classify_startup(&contradictory, &second.roots).unwrap(),
            StartupDecision::RecoveryRequired
        );

        let partial = Harness::new();
        write_bundle(&partial.root.join("skill"), "old").unwrap();
        let (partial_plan, _) = partial.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("old"),
            after: Some("new"),
            recovery_required: true,
        }]);
        partial.persist(&partial_plan);
        let mut partial_stage = partial
            .store
            .load(partial_plan.content.operation_id)
            .unwrap();
        let final_path = partial
            .roots
            .authorize(&partial_plan.content.steps[0].path)
            .unwrap()
            .path()
            .to_path_buf();
        let stage_path = owned_sibling(
            &final_path,
            partial_plan.content.operation_id,
            ArtifactKind::Stage,
        )
        .unwrap();
        fs::create_dir(&stage_path).unwrap();
        fs::write(stage_path.join("partial.tmp"), b"incomplete").unwrap();
        partial_stage.steps[0].stage_path = Some(stage_path.to_string_lossy().into_owned());
        partial_stage.steps[0]
            .stage
            .record_intent(UtcTimestamp::now())
            .unwrap();
        partial_stage
            .journal
            .transition(OperationState::Preflighted, UtcTimestamp::now())
            .unwrap();
        partial_stage
            .journal
            .transition(OperationState::Snapshotted, UtcTimestamp::now())
            .unwrap();
        assert_eq!(
            classify_startup(&partial_stage, &partial.roots).unwrap(),
            StartupDecision::DiscardStagingAndFailNoWrites
        );

        fs::remove_dir_all(&stage_path).unwrap();
        write_bundle(&stage_path, "new").unwrap();
        let staged_actual = capture_raw_path(
            &stage_path,
            &partial_plan.content.steps[0].after,
            partial_plan.content.bundle_caps,
        )
        .unwrap();
        partial_stage.steps[0]
            .stage
            .record_observed(staged_actual, UtcTimestamp::now())
            .unwrap();
        partial.store.write_step(&partial_stage.steps[0]).unwrap();
        partial_stage
            .journal
            .transition(OperationState::Staged, UtcTimestamp::now())
            .unwrap();
        partial_stage
            .journal
            .transition(OperationState::Committing, UtcTimestamp::now())
            .unwrap();
        partial.store.write_journal(&partial_stage.journal).unwrap();
        let committing = partial
            .store
            .load(partial_plan.content.operation_id)
            .unwrap();
        let before_classification = tree_bytes(&partial.root);
        assert_eq!(
            classify_startup(&committing, &partial.roots).unwrap(),
            StartupDecision::DiscardStagingAndFailNoWrites
        );
        assert_eq!(
            classify_startup(&committing, &partial.roots).unwrap(),
            StartupDecision::DiscardStagingAndFailNoWrites
        );
        assert_eq!(tree_bytes(&partial.root), before_classification);

        let mut contradictory_phases = committing;
        contradictory_phases.steps[0].stage = PhaseEvidence::not_started();
        assert_eq!(
            classify_startup(&contradictory_phases, &partial.roots).unwrap(),
            StartupDecision::RecoveryRequired
        );
        assert_eq!(tree_bytes(&partial.root), before_classification);
    }

    #[test]
    fn startup_driver_finalizes_or_fails_no_writes_and_terminal_replays_idempotently() {
        let no_write = Harness::new();
        let (plan, staged) = no_write.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Create,
            before: None,
            after: Some("new"),
            recovery_required: false,
        }]);
        no_write.persist(&plan);
        let executor = no_write.executor(staged, None, false);
        let first = executor.recover(plan.content.operation_id).unwrap();
        assert_eq!(first.outcome, OperationOutcome::FailedNoWrites);
        assert!(!first.replayed);
        let before_replay = tree_bytes(&no_write.root);
        let replay = executor.recover(plan.content.operation_id).unwrap();
        assert!(replay.replayed);
        assert_eq!(tree_bytes(&no_write.root), before_replay);

        let committed = Harness::new();
        write_bundle(&committed.root.join("skill"), "old").unwrap();
        let (plan, staged) = committed.plan(&[StepSpec {
            name: "skill",
            action: PlanAction::Replace,
            before: Some("old"),
            after: Some("new"),
            recovery_required: true,
        }]);
        committed.persist(&plan);
        let interrupted = committed.executor(staged, None, true);
        assert!(matches!(
            interrupted.execute(
                plan.content.operation_id,
                plan.plan_digest,
                &CancellationToken::default(),
            ),
            Err(OperationError::FinalizationInterrupted(_))
        ));
        let recovery = committed.executor(BTreeMap::new(), None, false);
        let completed = recovery.recover(plan.content.operation_id).unwrap();
        assert_eq!(completed.outcome, OperationOutcome::Succeeded);
        assert_eq!(read_bundle(&committed.root.join("skill")), "new");
        assert!(
            recovery
                .recover(plan.content.operation_id)
                .unwrap()
                .replayed
        );
    }

    #[test]
    fn explicit_bundle_subpath_proves_the_complete_container_shape() {
        let temporary = tempdir().unwrap();
        let container = temporary.path().join("container");
        let bundle = container.join("example");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("SKILL.md"), b"reviewed\n").unwrap();
        let digest = hash_bundle(&bundle, BundleCaps::default()).unwrap().digest;
        let adapter = AdapterId::new("nested-fingerprint-test", 1).unwrap();
        let mut expected = fingerprint(
            EntryKind::Directory,
            Some(digest),
            UtcTimestamp::now(),
            &adapter,
        );
        expected.bundle_subpath = Some(BundleRelativePath::parse("example").unwrap());

        let actual = capture_raw_path(&container, &expected, BundleCaps::default()).unwrap();
        assert!(fingerprint_matches(&expected, &actual));

        fs::write(container.join(".unexpected"), b"extra").unwrap();
        assert!(matches!(
            capture_raw_path(&container, &expected, BundleCaps::default()),
            Err(OperationError::FingerprintFailed(_))
        ));
        fs::remove_file(container.join(".unexpected")).unwrap();

        let mut wrong_subpath = expected.clone();
        wrong_subpath.bundle_subpath = Some(BundleRelativePath::parse("different").unwrap());
        assert!(matches!(
            capture_raw_path(&container, &wrong_subpath, BundleCaps::default()),
            Err(OperationError::FingerprintFailed(_))
        ));

        let mut wrong_digest = expected.clone();
        wrong_digest.bundle_digest = Some(crate::domain::BundleDigest::from_bytes([9; 32]));
        assert!(matches!(
            require_raw_path_fingerprint(
                &container,
                &wrong_digest,
                BundleCaps::default(),
                0,
                "nested digest changed"
            ),
            Err(OperationError::StalePlan { .. })
        ));

        #[cfg(unix)]
        {
            fs::remove_dir_all(&bundle).unwrap();
            let elsewhere = temporary.path().join("elsewhere");
            fs::create_dir(&elsewhere).unwrap();
            fs::write(elsewhere.join("SKILL.md"), b"reviewed\n").unwrap();
            std::os::unix::fs::symlink(&elsewhere, &bundle).unwrap();
            assert!(matches!(
                capture_raw_path(&container, &expected, BundleCaps::default()),
                Err(OperationError::FingerprintFailed(_))
            ));
        }
    }

    fn fingerprint(
        kind: EntryKind,
        digest: Option<crate::domain::BundleDigest>,
        captured_at: UtcTimestamp,
        adapter_id: &AdapterId,
    ) -> PathFingerprint {
        PathFingerprint {
            expected_kind: kind,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: digest,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: None,
            managed_deployment_id: None,
            captured_at,
            adapter_id: adapter_id.clone(),
        }
    }

    fn bundle_digest(content: &str) -> crate::domain::BundleDigest {
        let directory = tempdir().unwrap();
        write_bundle(directory.path(), content).unwrap();
        hash_bundle(directory.path(), BundleCaps::default())
            .unwrap()
            .digest
    }

    fn write_bundle(path: &Path, content: &str) -> io::Result<()> {
        fs::create_dir_all(path)?;
        let mut manifest = File::create(path.join("SKILL.md"))?;
        manifest.write_all(content.as_bytes())?;
        manifest.sync_all()?;
        sync_directory(path)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    fn write_bundle_exclusive(path: &Path, content: &str) -> io::Result<()> {
        fs::create_dir(path)?;
        write_bundle(path, content)
    }

    fn read_bundle(path: &Path) -> String {
        fs::read_to_string(path.join("SKILL.md")).unwrap()
    }

    fn visible_names(path: &Path) -> Vec<String> {
        let mut names = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn tree_bytes(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut entries = Vec::new();
        collect_tree(path, path, &mut entries);
        entries.sort();
        entries
    }

    fn collect_tree(root: &Path, path: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                entries.push((format!("{relative}/"), Vec::new()));
                collect_tree(root, &path, entries);
            } else {
                entries.push((relative, fs::read(path).unwrap()));
            }
        }
    }
}
