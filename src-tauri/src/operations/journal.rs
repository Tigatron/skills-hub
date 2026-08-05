use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    domain::{OperationId, OperationOutcome, OperationState, UtcTimestamp},
    filesystem::durable::{DurableWriteError, atomic_write, sync_directory},
};

use super::{OperationPlan, PathFingerprint, PlanDigest};

const JOURNAL_SCHEMA_VERSION: u16 = 1;
const STEP_SCHEMA_VERSION: u16 = 1;

/// Filesystem source of truth for immutable plans and crash-recovery evidence.
#[derive(Debug, Clone)]
pub struct OperationStore {
    operations_root: PathBuf,
}

impl OperationStore {
    /// Opens the exact `.manager/operations` directory below an existing manager directory.
    ///
    /// # Errors
    ///
    /// Returns an error when either directory is unsafe, unavailable, or a symbolic link.
    pub fn open(manager: &Path) -> Result<Self, JournalError> {
        let manager = verified_directory(manager)?;
        let selected_operations = manager.join("operations");
        match fs::symlink_metadata(&selected_operations) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(JournalError::UnsafeDirectory(selected_operations));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&selected_operations)?;
                sync_directory(&manager)?;
            }
            Err(error) => return Err(error.into()),
        }
        let operations_root = selected_operations.canonicalize()?;
        if !operations_root.starts_with(&manager) {
            return Err(JournalError::UnsafeDirectory(operations_root));
        }
        Ok(Self { operations_root })
    }

    #[must_use]
    pub fn operations_root(&self) -> &Path {
        &self.operations_root
    }

    #[must_use]
    pub fn operation_directory(&self, operation_id: OperationId) -> PathBuf {
        self.operations_root.join(operation_id.to_string())
    }

    #[must_use]
    pub fn plan_path(&self, operation_id: OperationId) -> PathBuf {
        self.operation_directory(operation_id).join("plan.json")
    }

    #[must_use]
    pub fn journal_path(&self, operation_id: OperationId) -> PathBuf {
        self.operation_directory(operation_id).join("journal.json")
    }

    #[must_use]
    pub fn step_path(&self, operation_id: OperationId, order: u32) -> PathBuf {
        self.operation_directory(operation_id)
            .join("steps")
            .join(format!("{order:06}.json"))
    }

    /// Persists a plan once, followed by initial numbered evidence and the state summary.
    /// Existing content for the same Operation ID is accepted only when it is byte-for-byte
    /// the same canonical plan.
    ///
    /// # Errors
    ///
    /// Returns an error for an ID collision, invalid plan, unsafe layout, or durability failure.
    pub fn persist_new_plan(
        &self,
        plan: &OperationPlan,
        now: UtcTimestamp,
    ) -> Result<StoredOperation, JournalError> {
        plan.verify_digest()
            .map_err(|error| JournalError::InvalidPlan(error.to_string()))?;
        let directory = self.operation_directory(plan.content.operation_id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {
                let stored = self.load(plan.content.operation_id)?;
                if stored.plan == *plan {
                    return Ok(stored);
                }
                return Err(JournalError::OperationIdCollision(
                    plan.content.operation_id,
                ));
            }
            Ok(_) => return Err(JournalError::UnsafeDirectory(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let temporary = self.operations_root.join(format!(
            ".{}.{}.tmp",
            plan.content.operation_id,
            Uuid::now_v7()
        ));
        fs::create_dir(&temporary)?;
        sync_directory(&self.operations_root)?;
        let result: Result<StoredOperation, JournalError> = (|| {
            let steps_directory = temporary.join("steps");
            fs::create_dir(&steps_directory)?;
            sync_directory(&temporary)?;
            atomic_write(
                &temporary.join("plan.json"),
                &plan
                    .canonical_json()
                    .map_err(|error| JournalError::InvalidPlan(error.to_string()))?,
            )?;

            let mut steps = Vec::with_capacity(plan.content.steps.len());
            for step in &plan.content.steps {
                let evidence = StepJournal::new(plan, step.order, now);
                write_json(
                    &steps_directory.join(format!("{:06}.json", step.order)),
                    &evidence,
                )?;
                steps.push(evidence);
            }
            let journal = OperationJournal::new(plan, now);
            write_json(&temporary.join("journal.json"), &journal)?;
            sync_directory(&temporary)?;
            fs::rename(&temporary, &directory)?;
            sync_directory(&self.operations_root)?;
            Ok(StoredOperation {
                plan: plan.clone(),
                journal,
                steps,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&temporary);
            let _ = sync_directory(&self.operations_root);
        }
        let stored = result?;
        Ok(stored)
    }

    /// Loads and validates all durable evidence for one Operation.
    ///
    /// # Errors
    ///
    /// Returns an error when evidence is missing, malformed, contradictory, or unsafe.
    pub fn load(&self, operation_id: OperationId) -> Result<StoredOperation, JournalError> {
        let directory = self.verified_operation_directory(operation_id)?;
        let plan = read_canonical_plan(&directory.join("plan.json"))?;
        plan.verify_digest()
            .map_err(|error| JournalError::InvalidPlan(error.to_string()))?;
        if plan.content.operation_id != operation_id {
            return Err(JournalError::ContradictoryEvidence(
                "plan Operation ID does not match its directory".to_owned(),
            ));
        }
        let journal: OperationJournal = read_json(&directory.join("journal.json"))?;
        journal.validate(&plan)?;
        let mut steps = Vec::with_capacity(plan.content.steps.len());
        for plan_step in &plan.content.steps {
            let evidence: StepJournal = read_json(&self.step_path(operation_id, plan_step.order))?;
            evidence.validate(&plan, plan_step.order)?;
            steps.push(evidence);
        }
        Ok(StoredOperation {
            plan,
            journal,
            steps,
        })
    }

    /// Atomically replaces the operation state summary.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid evidence or a durability failure.
    pub fn write_journal(&self, journal: &OperationJournal) -> Result<(), JournalError> {
        journal.validate_without_plan()?;
        let directory = self.verified_operation_directory(journal.operation_id)?;
        write_json(&directory.join("journal.json"), journal)
    }

    /// Atomically replaces one zero-padded step evidence file.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid evidence or a durability failure.
    pub fn write_step(&self, step: &StepJournal) -> Result<(), JournalError> {
        step.validate_without_plan()?;
        let directory = self.verified_operation_directory(step.operation_id)?;
        let steps = verified_directory(&directory.join("steps"))?;
        write_json(&steps.join(format!("{:06}.json", step.order)), step)
    }

    /// Returns Operation IDs with non-terminal journals, without interpreting their paths.
    ///
    /// # Errors
    ///
    /// Returns an error when an operation directory or journal is malformed.
    pub fn nonterminal_operation_ids(&self) -> Result<Vec<OperationId>, JournalError> {
        let mut nonterminal = Vec::new();
        for operation_id in self.operation_ids()? {
            if !self.load(operation_id)?.journal.state.is_terminal() {
                nonterminal.push(operation_id);
            }
        }
        Ok(nonterminal)
    }

    /// Returns every safely named Operation directory. Evidence is validated by callers with
    /// [`Self::load`] before it is interpreted.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe or malformed operation directory name.
    pub fn operation_ids(&self) -> Result<Vec<OperationId>, JournalError> {
        let mut operation_ids = Vec::new();
        for entry in fs::read_dir(&self.operations_root)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(JournalError::UnsafeDirectory(entry.path()));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| JournalError::InvalidOperationDirectory(entry.path()))?;
            let operation_id = OperationId::from_str(&name)
                .map_err(|_| JournalError::InvalidOperationDirectory(entry.path()))?;
            operation_ids.push(operation_id);
        }
        operation_ids.sort_unstable();
        Ok(operation_ids)
    }

    fn verified_operation_directory(
        &self,
        operation_id: OperationId,
    ) -> Result<PathBuf, JournalError> {
        let selected = self.operation_directory(operation_id);
        let directory = verified_directory(&selected)?;
        if directory.parent() != Some(self.operations_root.as_path()) {
            return Err(JournalError::UnsafeDirectory(directory));
        }
        Ok(directory)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOperation {
    pub plan: OperationPlan,
    pub journal: OperationJournal,
    pub steps: Vec<StepJournal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationJournal {
    pub schema_version: u16,
    pub operation_id: OperationId,
    pub plan_digest: PlanDigest,
    pub state: OperationState,
    pub outcome: Option<OperationOutcome>,
    pub snapshot_protections: Vec<SnapshotProtection>,
    pub failure: Option<OperationFailure>,
    pub cleanup_failures: Vec<String>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub finalized_at: Option<UtcTimestamp>,
}

impl OperationJournal {
    fn new(plan: &OperationPlan, now: UtcTimestamp) -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            operation_id: plan.content.operation_id,
            plan_digest: plan.plan_digest,
            state: OperationState::Planned,
            outcome: None,
            snapshot_protections: Vec::new(),
            failure: None,
            cleanup_failures: Vec::new(),
            created_at: now,
            updated_at: now,
            finalized_at: None,
        }
    }

    /// Advances the frozen domain state machine and updates durable timestamps.
    ///
    /// # Errors
    ///
    /// Returns an error for an illegal transition.
    pub fn transition(
        &mut self,
        next: OperationState,
        now: UtcTimestamp,
    ) -> Result<(), JournalError> {
        self.state = self
            .state
            .transition(next)
            .map_err(|error| JournalError::ContradictoryEvidence(error.to_string()))?;
        self.updated_at = now.max(self.updated_at);
        Ok(())
    }

    pub(crate) fn validate(&self, plan: &OperationPlan) -> Result<(), JournalError> {
        self.validate_without_plan()?;
        if self.operation_id != plan.content.operation_id || self.plan_digest != plan.plan_digest {
            return Err(JournalError::ContradictoryEvidence(
                "journal does not identify its immutable plan".to_owned(),
            ));
        }
        for protection in &self.snapshot_protections {
            let Some(step) = plan
                .content
                .steps
                .iter()
                .find(|step| step.order == protection.step_order)
            else {
                return Err(JournalError::ContradictoryEvidence(
                    "Snapshot protection identifies an unknown plan step".to_owned(),
                ));
            };
            if !step.is_destructive() || protection.before != step.before {
                return Err(JournalError::ContradictoryEvidence(
                    "Snapshot protection does not identify the exact destructive before-version"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn validate_without_plan(&self) -> Result<(), JournalError> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema(self.schema_version));
        }
        if self.updated_at < self.created_at
            || self
                .finalized_at
                .is_some_and(|value| value < self.updated_at)
        {
            return Err(JournalError::ContradictoryEvidence(
                "journal timestamps are out of order".to_owned(),
            ));
        }
        let valid_terminal = match self.state {
            OperationState::Finalized => {
                self.outcome == Some(OperationOutcome::Succeeded) && self.finalized_at.is_some()
            }
            OperationState::Failed => {
                matches!(
                    self.outcome,
                    Some(
                        OperationOutcome::CancelledNoWrites
                            | OperationOutcome::FailedNoWrites
                            | OperationOutcome::FailedRolledBack
                    )
                ) && self.finalized_at.is_some()
            }
            OperationState::RecoveryRequired => {
                self.outcome == Some(OperationOutcome::RecoveryRequired)
                    && self.finalized_at.is_some()
            }
            _ => self.outcome.is_none() && self.finalized_at.is_none(),
        };
        if !valid_terminal {
            return Err(JournalError::ContradictoryEvidence(
                "journal terminal state, outcome, and finalization disagree".to_owned(),
            ));
        }
        let mut protected_steps = BTreeSet::new();
        for protection in &self.snapshot_protections {
            if protection.reference.trim().is_empty()
                || !protected_steps.insert(protection.step_order)
            {
                return Err(JournalError::ContradictoryEvidence(
                    "Snapshot protections contain an empty reference or duplicate step".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Durable attestation that one protected Snapshot reference contains an exact before-version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotProtection {
    pub step_order: u32,
    pub reference: String,
    pub before: PathFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationFailure {
    pub code: String,
    pub summary: String,
    pub failed_step: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepJournal {
    pub schema_version: u16,
    pub operation_id: OperationId,
    pub plan_digest: PlanDigest,
    pub order: u32,
    pub stage_path: Option<String>,
    pub backup_path: Option<String>,
    pub rollback_path: Option<String>,
    pub rollback_source: Option<PathFingerprint>,
    pub stage: PhaseEvidence,
    pub commit: PhaseEvidence,
    pub verify: PhaseEvidence,
    pub rollback: PhaseEvidence,
    pub updated_at: UtcTimestamp,
}

impl StepJournal {
    fn new(plan: &OperationPlan, order: u32, now: UtcTimestamp) -> Self {
        Self {
            schema_version: STEP_SCHEMA_VERSION,
            operation_id: plan.content.operation_id,
            plan_digest: plan.plan_digest,
            order,
            stage_path: None,
            backup_path: None,
            rollback_path: None,
            rollback_source: None,
            stage: PhaseEvidence::not_started(),
            commit: PhaseEvidence::not_started(),
            verify: PhaseEvidence::not_started(),
            rollback: PhaseEvidence::not_started(),
            updated_at: now,
        }
    }

    pub(crate) fn validate(
        &self,
        plan: &OperationPlan,
        expected_order: u32,
    ) -> Result<(), JournalError> {
        self.validate_without_plan()?;
        if self.operation_id != plan.content.operation_id
            || self.plan_digest != plan.plan_digest
            || self.order != expected_order
        {
            return Err(JournalError::ContradictoryEvidence(
                "numbered step does not identify its immutable plan position".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_without_plan(&self) -> Result<(), JournalError> {
        if self.schema_version != STEP_SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema(self.schema_version));
        }
        self.stage.validate()?;
        self.commit.validate()?;
        self.verify.validate()?;
        self.rollback.validate()?;
        if (self.stage.status == PhaseStatus::NotStarted
            || self.stage.status == PhaseStatus::NotRequired)
            && self.stage_path.is_some()
        {
            return Err(JournalError::ContradictoryEvidence(
                "stage path exists without a durable stage intent".to_owned(),
            ));
        }
        if (self.commit.status == PhaseStatus::NotStarted
            || self.commit.status == PhaseStatus::NotRequired)
            && self.backup_path.is_some()
        {
            return Err(JournalError::ContradictoryEvidence(
                "backup path exists without a durable commit intent".to_owned(),
            ));
        }
        if (self.rollback.status == PhaseStatus::NotStarted
            || self.rollback.status == PhaseStatus::NotRequired)
            && (self.rollback_path.is_some() || self.rollback_source.is_some())
        {
            return Err(JournalError::ContradictoryEvidence(
                "rollback artifact evidence exists without a durable rollback intent".to_owned(),
            ));
        }
        if self.rollback_path.is_some() != self.rollback_source.is_some() {
            return Err(JournalError::ContradictoryEvidence(
                "rollback path and source fingerprint must be persisted together".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhaseEvidence {
    pub status: PhaseStatus,
    pub intent_at: Option<UtcTimestamp>,
    pub observed_at: Option<UtcTimestamp>,
    pub actual: Option<PathFingerprint>,
}

impl PhaseEvidence {
    #[must_use]
    pub const fn not_started() -> Self {
        Self {
            status: PhaseStatus::NotStarted,
            intent_at: None,
            observed_at: None,
            actual: None,
        }
    }

    #[must_use]
    pub const fn not_required() -> Self {
        Self {
            status: PhaseStatus::NotRequired,
            intent_at: None,
            observed_at: None,
            actual: None,
        }
    }

    pub(crate) fn record_intent(&mut self, now: UtcTimestamp) -> Result<(), JournalError> {
        if self.status != PhaseStatus::NotStarted {
            return Err(JournalError::ContradictoryEvidence(
                "phase intent was persisted more than once".to_owned(),
            ));
        }
        self.status = PhaseStatus::IntentPersisted;
        self.intent_at = Some(now);
        Ok(())
    }

    pub(crate) fn record_observed(
        &mut self,
        actual: PathFingerprint,
        now: UtcTimestamp,
    ) -> Result<(), JournalError> {
        let intent_at = self.intent_at.ok_or_else(|| {
            JournalError::ContradictoryEvidence("phase completion has no durable intent".to_owned())
        })?;
        if self.status != PhaseStatus::IntentPersisted {
            return Err(JournalError::ContradictoryEvidence(
                "phase completion is out of order".to_owned(),
            ));
        }
        let now = now.max(intent_at);
        self.status = PhaseStatus::ObservedComplete;
        self.observed_at = Some(now);
        self.actual = Some(actual);
        Ok(())
    }

    fn validate(&self) -> Result<(), JournalError> {
        let valid = match self.status {
            PhaseStatus::NotStarted | PhaseStatus::NotRequired => {
                self.intent_at.is_none() && self.observed_at.is_none() && self.actual.is_none()
            }
            PhaseStatus::IntentPersisted => {
                self.intent_at.is_some() && self.observed_at.is_none() && self.actual.is_none()
            }
            PhaseStatus::ObservedComplete => {
                self.intent_at.is_some()
                    && self.observed_at.is_some()
                    && self.actual.is_some()
                    && self.observed_at >= self.intent_at
            }
        };
        if !valid {
            return Err(JournalError::ContradictoryEvidence(
                "phase status and evidence disagree".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    NotStarted,
    NotRequired,
    IntentPersisted,
    ObservedComplete,
}

fn verified_directory(path: &Path) -> Result<PathBuf, JournalError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::UnsafeDirectory(path.to_path_buf()));
    }
    path.canonicalize().map_err(JournalError::Io)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, JournalError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|source| JournalError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })
}

fn read_canonical_plan(path: &Path) -> Result<OperationPlan, JournalError> {
    let bytes = fs::read(path)?;
    let plan: OperationPlan =
        serde_json::from_slice(&bytes).map_err(|source| JournalError::InvalidJson {
            path: path.to_path_buf(),
            source,
        })?;
    let canonical = plan
        .canonical_json()
        .map_err(|error| JournalError::InvalidPlan(error.to_string()))?;
    if bytes != canonical {
        return Err(JournalError::InvalidPlan(
            "persisted plan bytes are not the canonical immutable representation".to_owned(),
        ));
    }
    Ok(plan)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), JournalError> {
    let bytes = serde_json::to_vec(value)?;
    atomic_write(path, &bytes)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("operation journal filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("operation journal durability failed: {0}")]
    Durability(String),
    #[error("operation journal JSON serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("operation journal JSON is invalid at {path:?}: {source}")]
    InvalidJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("operation journal directory is unsafe: {0:?}")]
    UnsafeDirectory(PathBuf),
    #[error("operation directory name is not a valid Operation ID: {0:?}")]
    InvalidOperationDirectory(PathBuf),
    #[error("Operation ID {0} already identifies a different immutable plan")]
    OperationIdCollision(OperationId),
    #[error("operation journal schema version {0} is unsupported")]
    UnsupportedSchema(u16),
    #[error("immutable Operation Plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("operation evidence is contradictory: {0}")]
    ContradictoryEvidence(String),
}

impl From<DurableWriteError> for JournalError {
    fn from(error: DurableWriteError) -> Self {
        Self::Durability(error.to_string())
    }
}
