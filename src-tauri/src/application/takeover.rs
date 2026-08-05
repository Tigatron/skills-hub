use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use specta::Type;
use thiserror::Error;

use crate::{
    application::activity::ActivityService,
    domain::{
        ActivityId, BundleRelativePath, DeploymentHealth, DeploymentId, DeploymentMode,
        DurationMillis, ObservationId, OperationId, OperationOutcome, OperationState, SkillId,
        SkillLifecycle, SnapshotId, TargetId, UtcTimestamp,
    },
    filesystem::{
        AuthorizedRoot, BundleCaps, EntryKind, MetadataFingerprint, copy_bundle_exact, hash_bundle,
        validate_bundle_symlinks,
    },
    operations::{
        CancellationToken, OperationCoordinator, OperationError, OperationExecutor,
        OperationFailpoints, OperationFinalizer, OperationHookError, OperationIntent,
        OperationKind, OperationPlan, OperationPlanContent, OperationPlanner, OperationStore,
        OwnershipChoice, OwnershipDecision, PathFingerprint, PlanAction, PlanBuilder, PlanPath,
        PlanStep, RecoverySummary, SnapshotProtection, SnapshotRegistrar, SnapshotRegistration,
        StagingProvider, TakeoverDecision, TakeoverObservationEvidence, TakeoverObservationStatus,
        TakeoverPlanContext, TakeoverReplacementEvidence, TakeoverSkillEvidence,
        TakeoverTargetScope, TargetRoots,
    },
    persistence::{
        ActivityRecord, DeploymentManifest, DeploymentRecord, LocalSourceKind, ObjectRecord,
        ObservationRecord, OpenVault, OperationRecord, RepositoryError, SkillManifest,
        SkillManifestSource, SkillRecord, SkillRevisionRecord, SkillSourceRecord,
        SnapshotItemRecord, SnapshotRecord, SourceConfidence, TakeoverProjection, TargetRecord,
    },
};

const PREVIEW_LIMIT: u64 = 256 * 1024;
const SNAPSHOT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeepExternalRequest {
    pub observation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeepExternalResult {
    pub observation_id: String,
    pub kept_external: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverDecisionDto {
    AddToVault,
    AddAndManage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentModeDto {
    Symlink,
    ManagedCopy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectedLocationRequest {
    pub observation_id: String,
    pub mode: DeploymentModeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeoverPlanRequest {
    pub source_observation_id: String,
    pub decision: TakeoverDecisionDto,
    pub selected_locations: Vec<SelectedLocationRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecuteOperationRequest {
    pub operation_id: String,
    pub plan_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationIdRequest {
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillIdRequest {
    pub skill_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillPreviewRequest {
    pub skill_id: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ObservationEvidenceView {
    pub observation_id: String,
    pub path: String,
    pub canonical_path: Option<String>,
    pub digest: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SelectedReplacementView {
    pub observation_id: String,
    pub target_id: String,
    pub deployment_id: String,
    pub target_scope: String,
    pub path: String,
    pub requested_mode: DeploymentModeDto,
    pub resolved_mode: DeploymentModeDto,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverPlanView {
    pub operation_id: String,
    pub plan_digest: String,
    pub expires_at: String,
    pub decision: TakeoverDecisionDto,
    pub skill_id: String,
    pub observations: Vec<ObservationEvidenceView>,
    pub reviewed_digest: String,
    pub working_path: String,
    pub baseline_object_path: String,
    pub manifest_path: String,
    pub selected_replacements: Vec<SelectedReplacementView>,
    pub entry_count: u32,
    pub byte_count: u32,
    pub blockers: Vec<String>,
    pub recovery_summary: String,
    pub recovery_count: u32,
    pub cross_volume_consequence: Option<String>,
    pub execution_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OperationView {
    pub operation_id: String,
    pub plan_digest: String,
    pub state: String,
    pub outcome: Option<String>,
    pub failure: Option<String>,
    pub recovery: Vec<String>,
    pub context: TakeoverOperationContextView,
    pub review: TakeoverPlanView,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverOperationContextView {
    pub decision: TakeoverDecisionDto,
    pub source_observation_id: String,
    pub skill_id: String,
    pub working_path: String,
    pub selected_observation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OperationCancelResult {
    pub operation_id: String,
    pub cancellation_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TextPreview {
    pub skill_id: String,
    pub relative_path: String,
    pub size: u32,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SkillDetail {
    pub skill_id: String,
    pub display_name: String,
    pub deployment_name: String,
    pub working_path: String,
    pub working_digest: String,
    pub baseline_digest: String,
    pub ownership: String,
    pub lifecycle: String,
    pub source_paths: Vec<String>,
    pub deployment_paths: Vec<String>,
    pub observation_paths: Vec<String>,
    pub conflicts: Vec<String>,
    pub allowed_actions: Vec<String>,
}

#[derive(Debug, Error)]
pub enum TakeoverError {
    #[error("invalid {entity} ID: {detail}")]
    InvalidId {
        entity: &'static str,
        detail: String,
    },
    #[error("observation does not exist")]
    ObservationMissing,
    #[error("observation is stale, errored, already owned, or changed")]
    ObservationNotExternal,
    #[error("invalid takeover selection: {0}")]
    InvalidSelection(String),
    #[error("Skill does not exist")]
    SkillMissing,
    #[error("path is not a safe Bundle-relative path")]
    InvalidPreviewPath,
    #[error("preview path traverses a symbolic link or is not a regular file")]
    UnsafePreviewPath,
    #[error("preview exceeds 256 KiB")]
    PreviewTooLarge,
    #[error("preview changed while it was being read")]
    UnstablePreview,
    #[error("preview is not UTF-8")]
    PreviewNotUtf8,
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("persistence failed: {0}")]
    Persistence(#[from] RepositoryError),
    #[error("operation failed: {0}")]
    Operation(#[from] OperationError),
    #[error("operation evidence failed: {0}")]
    Journal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TakeoverBoundary {
    SourceValidated,
    SourceHashed,
    ObjectPublished,
}

trait TakeoverFailpoints: Send + Sync {
    fn check(&self, boundary: TakeoverBoundary) -> Result<(), OperationHookError>;
}

struct NoopTakeoverFailpoints;

impl TakeoverFailpoints for NoopTakeoverFailpoints {
    fn check(&self, _boundary: TakeoverBoundary) -> Result<(), OperationHookError> {
        Ok(())
    }
}

pub struct TakeoverService {
    vault: Arc<OpenVault>,
    coordinator: Arc<OperationCoordinator>,
    cancellations: Arc<Mutex<BTreeMap<OperationId, CancellationToken>>>,
    takeover_failpoints: Arc<dyn TakeoverFailpoints>,
    operation_failpoints: Arc<dyn OperationFailpoints>,
}

impl TakeoverService {
    #[must_use]
    #[cfg(test)]
    pub fn new(vault: Arc<OpenVault>) -> Self {
        Self::with_runtime(vault, Arc::new(OperationCoordinator::new()))
    }

    #[must_use]
    pub fn with_runtime(vault: Arc<OpenVault>, coordinator: Arc<OperationCoordinator>) -> Self {
        Self {
            vault,
            coordinator,
            cancellations: Arc::new(Mutex::new(BTreeMap::new())),
            takeover_failpoints: Arc::new(NoopTakeoverFailpoints),
            operation_failpoints: Arc::new(crate::operations::NoopOperationFailpoints),
        }
    }

    #[cfg(test)]
    fn with_failpoints(
        mut self,
        takeover: Arc<dyn TakeoverFailpoints>,
        operation: Arc<dyn OperationFailpoints>,
    ) -> Self {
        self.takeover_failpoints = takeover;
        self.operation_failpoints = operation;
        self
    }

    pub fn keep_external(
        &self,
        request: &KeepExternalRequest,
    ) -> Result<KeepExternalResult, TakeoverError> {
        let id = parse_observation_id(&request.observation_id)?;
        let observation = self
            .vault
            .repositories
            .observation(id)?
            .ok_or(TakeoverError::ObservationMissing)?;
        if observation.skill_id.is_some() || observation.status == "stale" {
            return Err(TakeoverError::ObservationNotExternal);
        }
        self.vault.repositories.set_setting(
            format!("keep_external:{id}"),
            &serde_json::json!(true),
            UtcTimestamp::now(),
        )?;
        Ok(KeepExternalResult {
            observation_id: id.to_string(),
            kept_external: true,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub fn plan_takeover(
        &self,
        request: TakeoverPlanRequest,
    ) -> Result<TakeoverPlanView, TakeoverError> {
        let TakeoverPlanRequest {
            source_observation_id,
            decision: requested_decision,
            selected_locations,
        } = request;
        let source_id = parse_observation_id(&source_observation_id)?;
        let source = self
            .vault
            .repositories
            .observation(source_id)?
            .ok_or(TakeoverError::ObservationMissing)?;
        let checked_source = inspect_observation(&source, BundleCaps::default())?;
        let mut selected = Vec::new();
        let mut selected_ids = BTreeSet::new();
        for choice in &selected_locations {
            let id = parse_observation_id(&choice.observation_id)?;
            if id == source_id {
                return Err(TakeoverError::InvalidSelection(
                    "the takeover source cannot also be a replacement".into(),
                ));
            }
            if !selected_ids.insert(id) {
                return Err(TakeoverError::InvalidSelection(
                    "duplicate selections are not allowed".into(),
                ));
            }
            let observation = self
                .vault
                .repositories
                .observation(id)?
                .ok_or(TakeoverError::ObservationMissing)?;
            let checked = inspect_observation(&observation, BundleCaps::default())?;
            if checked.canonical == checked_source.canonical
                || same_file_identity(checked.metadata, checked_source.metadata)
            {
                return Err(TakeoverError::InvalidSelection(
                    "a physical alias of the takeover source cannot be replaced".into(),
                ));
            }
            if checked.digest != checked_source.digest {
                return Err(TakeoverError::InvalidSelection(
                    "selected content must have the reviewed digest".into(),
                ));
            }
            let target_root = checked.canonical.parent().ok_or_else(|| {
                TakeoverError::InvalidSelection("selected path has no parent".into())
            })?;
            ensure_vault_target_disjoint(self.vault.paths.root(), target_root)?;
            selected.push((observation, checked, mode(choice.mode)));
        }
        match requested_decision {
            TakeoverDecisionDto::AddToVault if !selected.is_empty() => {
                return Err(TakeoverError::InvalidSelection(
                    "Add to Vault accepts no replacement".into(),
                ));
            }
            TakeoverDecisionDto::AddAndManage if selected.is_empty() => {
                return Err(TakeoverError::InvalidSelection(
                    "Add and manage requires an explicit location".into(),
                ));
            }
            _ => {}
        }
        selected.sort_by_key(|item| item.0.id);
        let operation_id = OperationId::generate();
        let skill_id = SkillId::generate();
        let working_target = TargetId::generate();
        let mut targets = vec![working_target];
        let mut deployments = Vec::new();
        let mut replacements = Vec::new();
        let mut planned_targets = BTreeMap::new();
        for (index, (observation, checked, deployment_mode)) in selected.iter().enumerate() {
            let parent = checked.canonical.parent().ok_or_else(|| {
                TakeoverError::InvalidSelection("selected path has no parent".into())
            })?;
            let scope = observation.scope.clone();
            let target_scope = checked_target_scope(observation)?;
            let existing = self.vault.repositories.target_by_identity(
                observation.adapter_id.clone(),
                scope.clone(),
                observation.project_id,
                parent,
            )?;
            let (
                target,
                target_root,
                target_canonical_root,
                project_id,
                is_override,
                is_custom,
                existing_target,
            ) = if let Some(value) = existing {
                let authorized = AuthorizedRoot::open(&value.root_path)
                    .map_err(|error| TakeoverError::InvalidSelection(error.to_string()))?;
                if value.scope != observation.scope
                    || value.project_id != observation.project_id
                    || value.canonical_root_path != parent
                    || authorized.canonical_path() != parent
                {
                    return Err(TakeoverError::InvalidSelection(
                        "existing target authority differs from the selected observation".into(),
                    ));
                }
                (
                    value.id,
                    value.root_path,
                    value.canonical_root_path,
                    value.project_id,
                    value.is_override,
                    value.is_custom,
                    true,
                )
            } else {
                let target = *planned_targets
                    .entry((
                        observation.adapter_id.to_string(),
                        scope,
                        parent.to_path_buf(),
                    ))
                    .or_insert_with(TargetId::generate);
                (
                    target,
                    parent.to_path_buf(),
                    parent.to_path_buf(),
                    observation.project_id,
                    false,
                    observation.source_root_kind == "custom",
                    false,
                )
            };
            let deployment = DeploymentId::generate();
            targets.push(target);
            deployments.push(deployment);
            replacements.push(TakeoverReplacementEvidence {
                observation_id: observation.id,
                target_id: target,
                deployment_id: deployment,
                adapter_id: observation.adapter_id.clone(),
                target_scope,
                target_root: target_root
                    .to_str()
                    .ok_or_else(|| {
                        TakeoverError::InvalidSelection(
                            "selected target root is not valid UTF-8".into(),
                        )
                    })?
                    .to_owned(),
                target_canonical_root: target_canonical_root
                    .to_str()
                    .ok_or_else(|| {
                        TakeoverError::InvalidSelection(
                            "canonical target root is not valid UTF-8".into(),
                        )
                    })?
                    .to_owned(),
                project_id,
                is_override,
                is_custom,
                existing_target,
                target_relative_path: BundleRelativePath::parse(
                    observation.deployment_name.as_str(),
                )
                .map_err(|e| TakeoverError::InvalidSelection(e.to_string()))?,
                deployment_mode: *deployment_mode,
                step_order: u32::try_from(index + 1)
                    .map_err(|_| TakeoverError::InvalidSelection("too many selections".into()))?,
            });
        }
        let now = UtcTimestamp::now();
        let intent = OperationIntent {
            operation_id,
            kind: OperationKind::TakeOver,
            selected_skill_ids: vec![skill_id],
            selected_target_ids: targets,
            selected_deployment_ids: deployments,
            ownership_choices: vec![OwnershipChoice {
                skill_id,
                decision: OwnershipDecision::TakeOver,
            }],
        };
        let builder = TakeoverBuilder {
            vault: Arc::clone(&self.vault),
            source,
            checked_source,
            selected,
            skill_id,
            working_target,
            replacements,
            decision: decision(requested_decision),
            created_at: now,
        };
        let store = OperationStore::open(self.vault.paths.manager())
            .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        let plan =
            OperationPlanner::new(store).plan(&intent, &builder, &CancellationToken::default())?;
        let context = plan
            .content
            .takeover
            .as_ref()
            .expect("takeover builder supplies context");
        Ok(plan_view(&plan, context))
    }

    pub fn execute_operation(
        &self,
        operation_id: &str,
        plan_digest: &str,
    ) -> Result<OperationView, TakeoverError> {
        let id = parse_operation_id(operation_id)?;
        let store = OperationStore::open(self.vault.paths.manager())
            .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        let stored = store
            .load(id)
            .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        if stored.plan.plan_digest.to_string() != plan_digest {
            return Err(TakeoverError::InvalidSelection(
                "plan digest differs from reviewed plan".into(),
            ));
        }
        let context = stored
            .plan
            .content
            .takeover
            .clone()
            .ok_or_else(|| TakeoverError::Journal("Operation is not a takeover".into()))?;
        if Path::new(&context.skill.vault_root) != self.vault.paths.root() {
            return Err(TakeoverError::Journal(
                "reviewed Vault authority differs from the open Vault".into(),
            ));
        }
        let mut roots = TargetRoots::new();
        roots.insert(
            context.skill.working_target_id,
            AuthorizedRoot::open(self.vault.paths.root())
                .map_err(|e| TakeoverError::Journal(e.to_string()))?,
        );
        for replacement in &context.replacements {
            let root = AuthorizedRoot::open(Path::new(&replacement.target_root))
                .map_err(|e| TakeoverError::Journal(e.to_string()))?;
            if root.canonical_path() != Path::new(&replacement.target_canonical_root) {
                return Err(TakeoverError::Journal(
                    "reviewed target authority changed".into(),
                ));
            }
            ensure_vault_target_disjoint(self.vault.paths.root(), root.canonical_path())?;
            roots.insert(replacement.target_id, root);
        }
        let token = CancellationToken::default();
        self.cancellations
            .lock()
            .map_err(|_| TakeoverError::Journal("cancellation registry is poisoned".into()))?
            .insert(id, token.clone());
        let hooks = Arc::new(TakeoverHooks {
            vault: Arc::clone(&self.vault),
            store: store.clone(),
            failpoints: Arc::clone(&self.takeover_failpoints),
        });
        let executor = OperationExecutor::new(
            store,
            Arc::clone(&self.coordinator),
            roots,
            hooks.clone(),
            hooks.clone(),
            hooks,
        )
        .with_failpoints(Arc::clone(&self.operation_failpoints));
        let result = executor.execute(id, stored.plan.plan_digest, &token);
        self.cancellations
            .lock()
            .map_err(|_| TakeoverError::Journal("cancellation registry is poisoned".into()))?
            .remove(&id);
        if result.is_err() {
            let _ = ActivityService::new(
                self.vault.repositories.clone(),
                OperationStore::open(self.vault.paths.manager())
                    .map_err(|error| TakeoverError::Journal(error.to_string()))?,
            )
            .project_terminal_operation(id);
        }
        let execution = result?;
        let mut view = self.operation_view(id)?;
        view.replayed = execution.replayed;
        Ok(view)
    }

    pub fn cancel(&self, operation_id: &str) -> Result<OperationCancelResult, TakeoverError> {
        let id = parse_operation_id(operation_id)?;
        let requested = self
            .cancellations
            .lock()
            .map_err(|_| TakeoverError::Journal("cancellation registry is poisoned".into()))?
            .get(&id)
            .is_some_and(|token| {
                token.cancel();
                true
            });
        Ok(OperationCancelResult {
            operation_id: id.to_string(),
            cancellation_requested: requested,
        })
    }

    pub fn get_operation(&self, operation_id: &str) -> Result<OperationView, TakeoverError> {
        self.operation_view(parse_operation_id(operation_id)?)
    }

    /// Recovers one takeover using the exact roots sealed in its persisted plan.
    pub fn recover_operation(
        &self,
        id: OperationId,
    ) -> Result<crate::operations::OperationExecution, TakeoverError> {
        let store = OperationStore::open(self.vault.paths.manager())
            .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        let stored = store
            .load(id)
            .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        let context = stored
            .plan
            .content
            .takeover
            .as_ref()
            .ok_or_else(|| TakeoverError::Journal("Operation is not a takeover".into()))?;
        if Path::new(&context.skill.vault_root) != self.vault.paths.root() {
            return Err(TakeoverError::Journal(
                "reviewed Vault authority differs from the open Vault".into(),
            ));
        }
        let mut roots = TargetRoots::new();
        roots.insert(
            context.skill.working_target_id,
            AuthorizedRoot::open(self.vault.paths.root())
                .map_err(|e| TakeoverError::Journal(e.to_string()))?,
        );
        for replacement in &context.replacements {
            let root = AuthorizedRoot::open(Path::new(&replacement.target_root))
                .map_err(|e| TakeoverError::Journal(e.to_string()))?;
            if root.canonical_path() != Path::new(&replacement.target_canonical_root) {
                return Err(TakeoverError::Journal(
                    "reviewed target authority changed".into(),
                ));
            }
            ensure_vault_target_disjoint(self.vault.paths.root(), root.canonical_path())?;
            roots.insert(replacement.target_id, root);
        }
        let hooks = Arc::new(TakeoverHooks {
            vault: Arc::clone(&self.vault),
            store: store.clone(),
            failpoints: Arc::clone(&self.takeover_failpoints),
        });
        OperationExecutor::new(
            store,
            Arc::clone(&self.coordinator),
            roots,
            hooks.clone(),
            hooks.clone(),
            hooks,
        )
        .with_failpoints(Arc::clone(&self.operation_failpoints))
        .recover(id)
        .map_err(Into::into)
    }

    fn operation_view(&self, id: OperationId) -> Result<OperationView, TakeoverError> {
        let stored = OperationStore::open(self.vault.paths.manager())
            .and_then(|store| store.load(id))
            .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        let takeover = stored
            .plan
            .content
            .takeover
            .as_ref()
            .ok_or_else(|| TakeoverError::Journal("Operation is not a takeover".into()))?;
        let review = plan_view(&stored.plan, takeover);
        Ok(OperationView {
            operation_id: id.to_string(),
            plan_digest: stored.plan.plan_digest.to_string(),
            state: format!("{:?}", stored.journal.state),
            outcome: stored.journal.outcome.map(|v| format!("{v:?}")),
            failure: stored.journal.failure.as_ref().map(|v| v.summary.clone()),
            recovery: stored
                .journal
                .snapshot_protections
                .iter()
                .map(|v| format!("step {}: {}", v.step_order, v.reference))
                .collect(),
            context: TakeoverOperationContextView {
                decision: decision_dto(takeover.decision),
                source_observation_id: takeover.source_observation_id.to_string(),
                skill_id: takeover.skill.skill_id.to_string(),
                working_path: takeover.skill.working_bundle_path.to_string(),
                selected_observation_ids: takeover
                    .replacements
                    .iter()
                    .map(|replacement| replacement.observation_id.to_string())
                    .collect(),
            },
            review,
            replayed: false,
        })
    }

    pub fn skill_detail(&self, skill_id: &str) -> Result<SkillDetail, TakeoverError> {
        let id = SkillId::from_str(skill_id).map_err(|e| invalid_id("Skill", &e))?;
        let skill = self
            .vault
            .repositories
            .skill(id)?
            .ok_or(TakeoverError::SkillMissing)?;
        let current = hash_bundle(
            &self.vault.paths.root().join(skill.working_path.as_str()),
            BundleCaps::default(),
        )
        .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        let baseline = self
            .vault
            .objects
            .verify(skill.baseline_digest)
            .map_err(|e| TakeoverError::Journal(e.to_string()))?;
        if current.digest != skill.working_digest || baseline.digest != skill.baseline_digest {
            return Err(TakeoverError::UnstablePreview);
        }
        let sources = self.vault.repositories.skill_sources(id)?;
        let deployments = self.vault.repositories.skill_deployments(id)?;
        let observations = self.vault.repositories.relevant_observations(
            skill.deployment_name.clone(),
            Some(skill.working_digest),
            500,
        )?;
        let active = deployments.iter().filter(|v| v.active).count();
        let conflicts = observations
            .iter()
            .filter(|v| v.status != "verified" || v.digest != Some(skill.working_digest))
            .map(|v| {
                format!(
                    "{}: {}",
                    v.display_path.display(),
                    v.error_summary.as_deref().unwrap_or(&v.status)
                )
            })
            .collect();
        let mut actions = vec!["preview".into(), "move_to_trash".into()];
        if active == 0 {
            actions.push("add_and_manage".into());
        }
        Ok(SkillDetail {
            skill_id: id.to_string(),
            display_name: skill.display_name,
            deployment_name: skill.deployment_name.to_string(),
            working_path: skill.working_path.to_string(),
            working_digest: skill.working_digest.to_string(),
            baseline_digest: skill.baseline_digest.to_string(),
            ownership: if active == 0 { "vaulted" } else { "managed" }.into(),
            lifecycle: format!("{:?}", skill.lifecycle).to_lowercase(),
            source_paths: sources
                .into_iter()
                .map(|v| v.path.to_string_lossy().into_owned())
                .collect(),
            deployment_paths: deployments
                .iter()
                .filter(|v| v.active)
                .map(|v| v.target_path.to_string_lossy().into_owned())
                .collect(),
            observation_paths: observations
                .iter()
                .map(|v| v.display_path.to_string_lossy().into_owned())
                .collect(),
            conflicts,
            allowed_actions: actions,
        })
    }

    pub fn preview(&self, skill_id: &str, relative: &str) -> Result<TextPreview, TakeoverError> {
        let id = SkillId::from_str(skill_id).map_err(|e| invalid_id("Skill", &e))?;
        let skill = self
            .vault
            .repositories
            .skill(id)?
            .ok_or(TakeoverError::SkillMissing)?;
        let relative =
            BundleRelativePath::parse(relative).map_err(|_| TakeoverError::InvalidPreviewPath)?;
        let (content, size) = read_stable_text(
            &self.vault.paths.root().join(skill.working_path.as_str()),
            &relative,
        )?;
        Ok(TextPreview {
            skill_id: id.to_string(),
            relative_path: relative.to_string(),
            size: u32::try_from(size).unwrap_or(u32::MAX),
            content,
        })
    }
}

#[derive(Clone)]
struct CheckedObservation {
    canonical: PathBuf,
    metadata: MetadataFingerprint,
    digest: crate::domain::BundleDigest,
    stats: crate::filesystem::BundleStats,
}

fn inspect_observation(
    observation: &ObservationRecord,
    caps: BundleCaps,
) -> Result<CheckedObservation, TakeoverError> {
    if observation.status != "verified"
        || observation.skill_id.is_some()
        || observation.stale_at.is_some()
        || observation.display_path.to_str().is_none()
    {
        return Err(TakeoverError::ObservationNotExternal);
    }
    let metadata = fs::symlink_metadata(&observation.display_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TakeoverError::ObservationNotExternal);
    }
    let canonical = observation.display_path.canonicalize()?;
    if canonical.to_str().is_none() {
        return Err(TakeoverError::ObservationNotExternal);
    }
    if observation
        .canonical_path
        .as_ref()
        .is_some_and(|value| value != &canonical)
    {
        return Err(TakeoverError::ObservationNotExternal);
    }
    validate_bundle_symlinks(&canonical, caps)
        .map_err(|e| TakeoverError::Journal(e.to_string()))?;
    let hashed =
        hash_bundle(&canonical, caps).map_err(|e| TakeoverError::Journal(e.to_string()))?;
    if observation.digest != Some(hashed.digest) {
        return Err(TakeoverError::ObservationNotExternal);
    }
    Ok(CheckedObservation {
        canonical,
        metadata: MetadataFingerprint::from_metadata(&metadata),
        digest: hashed.digest,
        stats: hashed.stats,
    })
}

struct TakeoverBuilder {
    vault: Arc<OpenVault>,
    source: ObservationRecord,
    checked_source: CheckedObservation,
    selected: Vec<(ObservationRecord, CheckedObservation, DeploymentMode)>,
    skill_id: SkillId,
    working_target: TargetId,
    replacements: Vec<TakeoverReplacementEvidence>,
    decision: TakeoverDecision,
    created_at: UtcTimestamp,
}

impl PlanBuilder for TakeoverBuilder {
    #[allow(clippy::too_many_lines)]
    fn build_content(
        &self,
        intent: &OperationIntent,
        cancellation: &CancellationToken,
    ) -> Result<OperationPlanContent, OperationError> {
        cancellation.check()?;
        let root = AuthorizedRoot::open(self.vault.paths.root())
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let container = BundleRelativePath::parse(&format!("skills/{}", self.skill_id))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let working_bundle = BundleRelativePath::parse(&format!(
            "skills/{}/{}",
            self.skill_id, self.source.deployment_name
        ))
        .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let working = root
            .authorize(&container)
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let adapter = self.source.adapter_id.clone();
        let before_absent = fingerprint(
            adapter.clone(),
            EntryKind::Absent,
            None,
            None,
            None,
            None,
            None,
            self.created_at,
        );
        let mut after_working = fingerprint(
            adapter.clone(),
            EntryKind::Directory,
            None,
            None,
            Some(self.checked_source.digest),
            Some(self.skill_id),
            None,
            self.created_at,
        );
        after_working.bundle_subpath = Some(
            BundleRelativePath::parse(self.source.deployment_name.as_str())
                .map_err(|error| OperationError::InvalidPlan(error.to_string()))?,
        );
        let mut steps = vec![PlanStep::new(
            PlanAction::Create,
            PlanPath::from_authorized(self.working_target, &working)
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            None,
            None,
            before_absent,
            after_working,
            false,
        )];
        for ((observation, checked, deployment_mode), replacement) in
            self.selected.iter().zip(&self.replacements)
        {
            let target_root = AuthorizedRoot::open(Path::new(&replacement.target_root))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
            if target_root.canonical_path() != Path::new(&replacement.target_canonical_root) {
                return Err(OperationError::InvalidPlan(
                    "selected target root authority changed".into(),
                ));
            }
            let path = target_root
                .authorize(&replacement.target_relative_path)
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
            let before = fingerprint(
                observation.adapter_id.clone(),
                EntryKind::Directory,
                None,
                Some(checked.metadata),
                Some(checked.digest),
                None,
                None,
                self.created_at,
            );
            let (kind, raw, digest) = match deployment_mode {
                DeploymentMode::Symlink => (
                    EntryKind::Symlink,
                    Some(
                        self.vault
                            .paths
                            .root()
                            .join(working_bundle.as_str())
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    None,
                ),
                DeploymentMode::ManagedCopy => {
                    (EntryKind::Directory, None, Some(self.checked_source.digest))
                }
            };
            let after = fingerprint(
                observation.adapter_id.clone(),
                kind,
                raw,
                None,
                digest,
                Some(self.skill_id),
                Some(replacement.deployment_id),
                self.created_at,
            );
            steps.push(PlanStep::new(
                PlanAction::Replace,
                PlanPath::from_authorized(replacement.target_id, &path)
                    .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
                Some(*deployment_mode),
                Some(*deployment_mode),
                before,
                after,
                true,
            ));
        }
        let mut observations = self
            .vault
            .repositories
            .relevant_observations(
                self.source.deployment_name.clone(),
                Some(self.checked_source.digest),
                500,
            )
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        for explicit in std::iter::once(&self.source).chain(self.selected.iter().map(|v| &v.0)) {
            if !observations.iter().any(|v| v.id == explicit.id) {
                observations.push(explicit.clone());
            }
        }
        observations.sort_by_key(|v| v.id);
        observations.dedup_by_key(|v| v.id);
        let evidence = observations
            .iter()
            .map(|v| {
                observation_evidence(
                    v,
                    std::iter::once((&self.source, &self.checked_source))
                        .chain(self.selected.iter().map(|x| (&x.0, &x.1)))
                        .find(|(o, _)| o.id == v.id)
                        .map(|(_, c)| c),
                )
            })
            .collect();
        let object_path = object_relative(self.checked_source.digest)
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let context = TakeoverPlanContext {
            decision: self.decision,
            source_observation_id: self.source.id,
            observations: evidence,
            skill: TakeoverSkillEvidence {
                skill_id: self.skill_id,
                display_name: self.source.deployment_name.to_string(),
                deployment_name: self.source.deployment_name.clone(),
                vault_root: self.vault.paths.root().to_string_lossy().into_owned(),
                working_target_id: self.working_target,
                working_container_path: container,
                working_bundle_path: working_bundle,
                manifest_path: BundleRelativePath::parse(&format!(
                    ".manager/manifests/skills/{}.json",
                    self.skill_id
                ))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
                baseline_digest: Some(self.checked_source.digest),
                baseline_object_path: Some(object_path),
                working_step_order: 0,
                activity_id: ActivityId::generate(),
                snapshot_id: (!self.replacements.is_empty()).then(SnapshotId::generate),
            },
            replacements: self.replacements.clone(),
        };
        let expires = self
            .created_at
            .checked_add(DurationMillis(300_000))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let vault_device = MetadataFingerprint::from_metadata(
            &fs::symlink_metadata(self.vault.paths.root()).map_err(|error| {
                OperationError::Filesystem {
                    context: "inspecting Vault volume during takeover planning",
                    source: error,
                }
            })?,
        )
        .device_id;
        let spans_filesystems = self.replacements.iter().any(|replacement| {
            fs::symlink_metadata(&replacement.target_root)
                .map(|metadata| MetadataFingerprint::from_metadata(&metadata).device_id)
                .is_ok_and(|device| device != vault_device)
        });
        let managed_copy_bytes: u64 = self
            .selected
            .iter()
            .filter(|(_, _, mode)| *mode == DeploymentMode::ManagedCopy)
            .map(|(_, checked, _)| checked.stats.regular_file_bytes)
            .sum();
        Ok(OperationPlanContent::new(
            intent.operation_id,
            intent.kind,
            self.created_at,
            expires,
            intent.selected_skill_ids.clone(),
            intent.selected_target_ids.clone(),
            intent.selected_deployment_ids.clone(),
            intent.ownership_choices.clone(),
            BundleCaps::default(),
            self.checked_source.stats,
            steps,
            Vec::new(),
            RecoverySummary {
                snapshot_count: u32::from(!self.replacements.is_empty()),
                estimated_staging_bytes: self
                    .checked_source
                    .stats
                    .regular_file_bytes
                    .saturating_add(managed_copy_bytes),
                estimated_snapshot_bytes: self
                    .selected
                    .iter()
                    .map(|v| v.1.stats.regular_file_bytes)
                    .sum(),
                estimated_rollback_bytes: self
                    .selected
                    .iter()
                    .map(|v| v.1.stats.regular_file_bytes)
                    .sum(),
                spans_filesystems,
            },
            if spans_filesystems {
                vec![
                    "Selected replacements span filesystems; each switch is atomic only within its target parent and operation-level rollback is compensating."
                        .to_owned(),
                ]
            } else {
                Vec::new()
            },
        )
        .with_takeover_context(context))
    }
}

fn observation_evidence(
    observation: &ObservationRecord,
    checked: Option<&CheckedObservation>,
) -> TakeoverObservationEvidence {
    TakeoverObservationEvidence {
        observation_id: observation.id,
        skill_id: observation.skill_id,
        adapter_id: observation.adapter_id.clone(),
        target_scope: if observation.scope == "project" {
            TakeoverTargetScope::Project
        } else {
            TakeoverTargetScope::Global
        },
        project_id: observation.project_id,
        source_root_kind: observation.source_root_kind.clone(),
        source_root_id: observation.source_root_id.clone(),
        display_path: observation.display_path.to_string_lossy().into_owned(),
        canonical_path: observation
            .canonical_path
            .as_ref()
            .map(|v| v.to_string_lossy().into_owned()),
        deployment_name: observation.deployment_name.clone(),
        bundle_digest: observation.digest,
        status: if observation.status == "verified" {
            TakeoverObservationStatus::Present
        } else {
            TakeoverObservationStatus::Error
        },
        error_code: observation.error_code.clone(),
        error_summary: observation.error_summary.clone(),
        observed_at: observation.observed_at,
        entry_kind: checked.map_or(EntryKind::Unsupported, |_| EntryKind::Directory),
        metadata: checked.map(|v| v.metadata),
        raw_symlink_target: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotEvidence {
    schema_version: u32,
    operation_id: OperationId,
    snapshot_id: SnapshotId,
    protections: Vec<SnapshotProtection>,
}

struct TakeoverHooks {
    vault: Arc<OpenVault>,
    store: OperationStore,
    failpoints: Arc<dyn TakeoverFailpoints>,
}

impl TakeoverHooks {
    fn revalidate_observation(
        &self,
        evidence: &TakeoverObservationEvidence,
        caps: BundleCaps,
    ) -> Result<(), OperationHookError> {
        let current = self
            .vault
            .repositories
            .observation(evidence.observation_id)
            .map_err(|error| hook(error.to_string()))?
            .ok_or_else(|| hook("reviewed observation no longer exists"))?;
        if current.skill_id != evidence.skill_id
            || current.adapter_id != evidence.adapter_id
            || current.scope
                != match evidence.target_scope {
                    TakeoverTargetScope::Global => "global",
                    TakeoverTargetScope::Project => "project",
                }
            || current.project_id != evidence.project_id
            || current.source_root_kind != evidence.source_root_kind
            || current.source_root_id != evidence.source_root_id
            || current.display_path.to_str() != Some(evidence.display_path.as_str())
            || current.canonical_path.as_deref().and_then(Path::to_str)
                != evidence.canonical_path.as_deref()
            || current.deployment_name != evidence.deployment_name
            || current.digest != evidence.bundle_digest
            || current.status != "verified"
            || current.error_code != evidence.error_code
            || current.error_summary != evidence.error_summary
            || current.observed_at != evidence.observed_at
            || current.stale_at.is_some()
        {
            return Err(hook("reviewed observation record changed"));
        }
        revalidate_evidence(evidence, caps, self.failpoints.as_ref())
    }

    fn revalidate_target(
        &self,
        evidence: &TakeoverReplacementEvidence,
    ) -> Result<(), OperationHookError> {
        let scope = match evidence.target_scope {
            TakeoverTargetScope::Global => "global",
            TakeoverTargetScope::Project => "project",
        };
        let canonical = Path::new(&evidence.target_canonical_root);
        let current = self
            .vault
            .repositories
            .target_by_identity(
                evidence.adapter_id.clone(),
                scope.into(),
                evidence.project_id,
                canonical,
            )
            .map_err(|error| hook(error.to_string()))?;
        match (evidence.existing_target, current) {
            (true, Some(current))
                if current.id == evidence.target_id
                    && current.adapter_id == evidence.adapter_id
                    && current.scope == scope
                    && current.root_path == Path::new(&evidence.target_root)
                    && current.canonical_root_path == canonical
                    && current.project_id == evidence.project_id
                    && current.is_override == evidence.is_override
                    && current.is_custom == evidence.is_custom =>
            {
                Ok(())
            }
            (false, None) => Ok(()),
            _ => Err(hook("reviewed target authority changed")),
        }
    }
}

impl StagingProvider for TakeoverHooks {
    fn stage(
        &self,
        plan: &OperationPlan,
        step: &PlanStep,
        staging_path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), OperationHookError> {
        cancellation.check().map_err(|e| hook(e.to_string()))?;
        let context = plan
            .content
            .takeover
            .as_ref()
            .ok_or_else(|| hook("missing takeover context"))?;
        let source = context
            .observations
            .iter()
            .find(|v| v.observation_id == context.source_observation_id)
            .ok_or_else(|| hook("missing source evidence"))?;
        self.revalidate_observation(source, plan.content.bundle_caps)?;
        let digest = source
            .bundle_digest
            .ok_or_else(|| hook("source has no digest"))?;
        let published = self
            .vault
            .objects
            .publish(
                plan.content.operation_id,
                Path::new(&source.display_path),
                Some(digest),
                UtcTimestamp::now(),
            )
            .map_err(|e| hook(e.to_string()))?;
        self.failpoints.check(TakeoverBoundary::ObjectPublished)?;
        self.vault
            .objects
            .verify(digest)
            .map_err(|e| hook(e.to_string()))?;
        self.revalidate_observation(source, plan.content.bundle_caps)?;
        cancellation.check().map_err(|e| hook(e.to_string()))?;
        if step.order == context.skill.working_step_order {
            fs::create_dir(staging_path).map_err(|e| hook(e.to_string()))?;
            let nested = staging_path.join(context.skill.deployment_name.as_str());
            let copied = copy_bundle_exact(
                &published.path.join("bundle"),
                &nested,
                plan.content.bundle_caps,
            )
            .map_err(|e| hook(e.to_string()))?;
            if copied.digest != digest {
                return Err(hook("staged working digest mismatch"));
            }
            return Ok(());
        }
        let replacement = context
            .replacements
            .iter()
            .find(|v| v.step_order == step.order)
            .ok_or_else(|| hook("missing replacement"))?;
        self.revalidate_target(replacement)?;
        let selected = context
            .observations
            .iter()
            .find(|v| v.observation_id == replacement.observation_id)
            .ok_or_else(|| hook("missing selected evidence"))?;
        self.revalidate_observation(selected, plan.content.bundle_caps)?;
        match replacement.deployment_mode {
            DeploymentMode::ManagedCopy => {
                let copied = copy_bundle_exact(
                    &published.path.join("bundle"),
                    staging_path,
                    plan.content.bundle_caps,
                )
                .map_err(|e| hook(e.to_string()))?;
                if copied.digest != digest {
                    return Err(hook("staged deployment digest mismatch"));
                }
            }
            DeploymentMode::Symlink => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    self.vault
                        .paths
                        .root()
                        .join(context.skill.working_bundle_path.as_str()),
                    staging_path,
                )
                .map_err(|e| hook(e.to_string()))?;
            }
        }
        Ok(())
    }
}

impl SnapshotRegistrar for TakeoverHooks {
    fn register(
        &self,
        plan: &OperationPlan,
        protected_steps: &[PlanStep],
        cancellation: &CancellationToken,
    ) -> Result<SnapshotRegistration, OperationHookError> {
        if protected_steps.is_empty() {
            return Ok(SnapshotRegistration::default());
        }
        let context = plan
            .content
            .takeover
            .as_ref()
            .ok_or_else(|| hook("missing takeover context"))?;
        let snapshot_id = context
            .skill
            .snapshot_id
            .ok_or_else(|| hook("missing snapshot ID"))?;
        let mut protections = Vec::new();
        for step in protected_steps {
            cancellation.check().map_err(|e| hook(e.to_string()))?;
            let digest = step
                .before
                .bundle_digest
                .ok_or_else(|| hook("destructive directory has no digest"))?;
            self.vault
                .objects
                .publish(
                    plan.content.operation_id,
                    Path::new(step.path.display_path()),
                    Some(digest),
                    UtcTimestamp::now(),
                )
                .map_err(|e| hook(e.to_string()))?;
            protections.push(SnapshotProtection {
                step_order: step.order,
                reference: format!("object:{digest}"),
                before: step.before.clone(),
            });
        }
        let evidence = SnapshotEvidence {
            schema_version: SNAPSHOT_SCHEMA,
            operation_id: plan.content.operation_id,
            snapshot_id,
            protections: protections.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&evidence).map_err(|e| hook(e.to_string()))?;
        let path = self
            .store
            .operation_directory(plan.content.operation_id)
            .join("takeover-snapshot.json");
        if path.exists() {
            let existing: SnapshotEvidence =
                serde_json::from_slice(&fs::read(&path).map_err(|e| hook(e.to_string()))?)
                    .map_err(|e| hook(e.to_string()))?;
            if existing != evidence {
                return Err(hook("snapshot evidence differs"));
            }
        } else {
            crate::filesystem::durable::atomic_write(&path, &bytes)
                .map_err(|e| hook(e.to_string()))?;
        }
        Ok(SnapshotRegistration { protections })
    }
}

impl OperationFinalizer for TakeoverHooks {
    fn publish_manifests(
        &self,
        plan: &OperationPlan,
        _journal: &crate::operations::OperationJournal,
    ) -> Result<(), OperationHookError> {
        let context = plan
            .content
            .takeover
            .as_ref()
            .ok_or_else(|| hook("missing takeover context"))?;
        let digest = context
            .skill
            .baseline_digest
            .ok_or_else(|| hook("missing baseline"))?;
        let working = self
            .vault
            .paths
            .root()
            .join(context.skill.working_bundle_path.as_str());
        if hash_bundle(&working, plan.content.bundle_caps)
            .map_err(|e| hook(e.to_string()))?
            .digest
            != digest
        {
            return Err(hook("final working digest mismatch"));
        }
        self.vault
            .objects
            .verify(digest)
            .map_err(|e| hook(e.to_string()))?;
        let source = context
            .observations
            .iter()
            .find(|v| v.observation_id == context.source_observation_id)
            .ok_or_else(|| hook("missing source"))?;
        let manifest = SkillManifest::new(
            context.skill.skill_id,
            context.skill.display_name.clone(),
            context.skill.deployment_name.clone(),
            digest,
            digest,
            plan.content.created_at,
            vec![SkillManifestSource {
                kind: LocalSourceKind::LocalObservation,
                path: PathBuf::from(&source.display_path),
                captured_at: source.observed_at,
                confidence: SourceConfidence::Observed,
            }],
        )
        .map_err(|e| hook(e.to_string()))?;
        self.vault
            .manifests
            .write_skill(&manifest)
            .map_err(|e| hook(e.to_string()))?;
        for replacement in &context.replacements {
            let target_path =
                Path::new(&replacement.target_root).join(replacement.target_relative_path.as_str());
            let deployment_name = replacement_deployment_name(replacement)?;
            self.vault
                .manifests
                .write_deployment(&DeploymentManifest {
                    schema_version: 1,
                    deployment_id: replacement.deployment_id,
                    skill_id: context.skill.skill_id,
                    target_id: replacement.target_id,
                    deployment_name,
                    mode: replacement.deployment_mode,
                    target_path,
                    expected_digest: digest,
                    expected_link_target: (replacement.deployment_mode == DeploymentMode::Symlink)
                        .then(|| working.clone()),
                    adapter_version: replacement.adapter_id.clone(),
                    last_finalized_operation_id: plan.content.operation_id,
                    verified_at: UtcTimestamp::now(),
                })
                .map_err(|e| hook(e.to_string()))?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn finalize_projection(
        &self,
        plan: &OperationPlan,
        journal: &crate::operations::OperationJournal,
    ) -> Result<(), OperationHookError> {
        let c = plan
            .content
            .takeover
            .as_ref()
            .ok_or_else(|| hook("missing takeover context"))?;
        let digest = c
            .skill
            .baseline_digest
            .ok_or_else(|| hook("missing digest"))?;
        let verified = self
            .vault
            .objects
            .verify(digest)
            .map_err(|e| hook(e.to_string()))?;
        let now = journal.updated_at;
        let source_ids: BTreeSet<_> = std::iter::once(c.source_observation_id)
            .chain(c.replacements.iter().map(|v| v.observation_id))
            .collect();
        let managed_observation_ids = c
            .replacements
            .iter()
            .map(|replacement| replacement.observation_id)
            .collect();
        let sources = c
            .observations
            .iter()
            .filter(|v| source_ids.contains(&v.observation_id))
            .map(|v| SkillSourceRecord {
                skill_id: c.skill.skill_id,
                kind: "local_observation".into(),
                path: PathBuf::from(&v.display_path),
                captured_at: v.observed_at,
                confidence: "observed".into(),
            })
            .collect();
        let targets = c
            .replacements
            .iter()
            .map(|v| TargetRecord {
                id: v.target_id,
                adapter_id: v.adapter_id.clone(),
                scope: match v.target_scope {
                    TakeoverTargetScope::Global => "global",
                    TakeoverTargetScope::Project => "project",
                }
                .into(),
                root_path: PathBuf::from(&v.target_root),
                canonical_root_path: PathBuf::from(&v.target_canonical_root),
                project_id: v.project_id,
                is_override: v.is_override,
                is_custom: v.is_custom,
                created_at: plan.content.created_at,
                updated_at: now,
            })
            .collect();
        let deployments = c
            .replacements
            .iter()
            .map(|v| {
                Ok(DeploymentRecord {
                    id: v.deployment_id,
                    skill_id: c.skill.skill_id,
                    target_id: v.target_id,
                    deployment_name: replacement_deployment_name(v)?,
                    target_path: Path::new(&v.target_root).join(v.target_relative_path.as_str()),
                    mode: v.deployment_mode,
                    expected_digest: digest,
                    expected_link_target: (v.deployment_mode == DeploymentMode::Symlink).then(
                        || {
                            self.vault
                                .paths
                                .root()
                                .join(c.skill.working_bundle_path.as_str())
                        },
                    ),
                    health: DeploymentHealth::Clean,
                    adapter_version: v.adapter_id.clone(),
                    active: true,
                    last_verified_at: Some(now),
                    last_operation_id: Some(plan.content.operation_id),
                    created_at: plan.content.created_at,
                    updated_at: now,
                })
            })
            .collect::<Result<Vec<_>, OperationHookError>>()?;
        let evidence = c
            .skill
            .snapshot_id
            .map(|_| read_snapshot(&self.store, plan.content.operation_id))
            .transpose()?;
        let (snapshot, snapshot_items) = match (c.skill.snapshot_id, evidence) {
            (Some(id), Some(e)) => (
                Some(SnapshotRecord {
                    id,
                    operation_id: plan.content.operation_id,
                    retention_state: "protected".into(),
                    protected: true,
                    created_at: plan.content.created_at,
                }),
                e.protections
                    .iter()
                    .enumerate()
                    .map(|(i, v)| SnapshotItemRecord {
                        snapshot_id: id,
                        ordinal: i,
                        digest: v.before.bundle_digest,
                        entry_fingerprint: serde_json::to_value(&v.before).ok(),
                        relation: "takeover_original".into(),
                    })
                    .collect(),
            ),
            _ => (None, Vec::new()),
        };
        self.vault.repositories.finalize_takeover(TakeoverProjection { operation: OperationRecord { id: plan.content.operation_id, plan_digest: plan.plan_digest.to_string(), operation_type: "takeover".into(), state: OperationState::Finalized, outcome: Some(OperationOutcome::Succeeded), recovery_state: None, journal_path: BundleRelativePath::parse(&format!(".manager/operations/{}/journal.json", plan.content.operation_id)).map_err(|e| hook(e.to_string()))?, created_at: plan.content.created_at, updated_at: now, finalized_at: Some(now) }, skill: SkillRecord { id: c.skill.skill_id, display_name: c.skill.display_name.clone(), deployment_name: c.skill.deployment_name.clone(), working_path: c.skill.working_bundle_path.clone(), working_digest: digest, baseline_digest: digest, lifecycle: SkillLifecycle::Active, created_at: plan.content.created_at, updated_at: now }, sources, object: ObjectRecord { digest, relative_path: c.skill.baseline_object_path.clone().ok_or_else(|| hook("missing object path"))?, entry_count: verified.entry_count, byte_count: verified.byte_count, verified_at: now }, revision: SkillRevisionRecord { skill_id: c.skill.skill_id, digest, kind: "takeover_baseline".into(), operation_id: Some(plan.content.operation_id), created_at: now }, targets, deployments, snapshot, snapshot_items, observation_ids: managed_observation_ids, activity: ActivityRecord { id: c.skill.activity_id, operation_id: Some(plan.content.operation_id), kind: "takeover".into(), state: "completed".into(), outcome: Some(OperationOutcome::Succeeded), summary: format!("Added {} to Vault", c.skill.display_name), details: serde_json::json!({"skillId": c.skill.skill_id, "decision": c.decision}), started_at: plan.content.created_at, completed_at: Some(now) } }).map_err(|e| hook(e.to_string()))
    }
}

fn read_snapshot(
    store: &OperationStore,
    id: OperationId,
) -> Result<SnapshotEvidence, OperationHookError> {
    serde_json::from_slice(
        &fs::read(store.operation_directory(id).join("takeover-snapshot.json"))
            .map_err(|e| hook(e.to_string()))?,
    )
    .map_err(|e| hook(e.to_string()))
}
fn hook(value: impl Into<String>) -> OperationHookError {
    OperationHookError::new(value.into())
}

fn replacement_deployment_name(
    replacement: &TakeoverReplacementEvidence,
) -> Result<crate::domain::DeploymentName, OperationHookError> {
    crate::domain::DeploymentName::parse(replacement.target_relative_path.as_str())
        .map_err(|error| hook(error.to_string()))
}

fn revalidate_evidence(
    e: &TakeoverObservationEvidence,
    caps: BundleCaps,
    failpoints: &dyn TakeoverFailpoints,
) -> Result<(), OperationHookError> {
    let path = Path::new(&e.display_path);
    let metadata = fs::symlink_metadata(path).map_err(|x| hook(x.to_string()))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || e.metadata != Some(MetadataFingerprint::from_metadata(&metadata))
    {
        return Err(hook("source metadata changed"));
    }
    let canonical = path.canonicalize().map_err(|x| hook(x.to_string()))?;
    if e.canonical_path
        .as_ref()
        .is_some_and(|v| Path::new(v) != canonical)
    {
        return Err(hook("source canonical path changed"));
    }
    failpoints.check(TakeoverBoundary::SourceValidated)?;
    validate_bundle_symlinks(&canonical, caps).map_err(|x| hook(x.to_string()))?;
    let digest = hash_bundle(&canonical, caps)
        .map_err(|x| hook(x.to_string()))?
        .digest;
    failpoints.check(TakeoverBoundary::SourceHashed)?;
    if digest
        != e.bundle_digest
            .ok_or_else(|| hook("missing reviewed digest"))?
    {
        return Err(hook("source digest changed"));
    }
    Ok(())
}

fn plan_view(plan: &OperationPlan, c: &TakeoverPlanContext) -> TakeoverPlanView {
    let stats = plan.content.observed_bundle_stats;
    TakeoverPlanView {
        operation_id: plan.content.operation_id.to_string(),
        plan_digest: plan.plan_digest.to_string(),
        expires_at: plan.content.expires_at.to_string(),
        decision: decision_dto(c.decision),
        skill_id: c.skill.skill_id.to_string(),
        observations: c
            .observations
            .iter()
            .map(|v| ObservationEvidenceView {
                observation_id: v.observation_id.to_string(),
                path: v.display_path.clone(),
                canonical_path: v.canonical_path.clone(),
                digest: v.bundle_digest.map(|x| x.to_string()),
                status: format!("{:?}", v.status).to_lowercase(),
                error: v.error_summary.clone(),
            })
            .collect(),
        reviewed_digest: c
            .skill
            .baseline_digest
            .expect("validated context")
            .to_string(),
        working_path: c.skill.working_bundle_path.to_string(),
        baseline_object_path: c
            .skill
            .baseline_object_path
            .as_ref()
            .expect("validated context")
            .to_string(),
        manifest_path: c.skill.manifest_path.to_string(),
        selected_replacements: c
            .replacements
            .iter()
            .map(|replacement| SelectedReplacementView {
                observation_id: replacement.observation_id.to_string(),
                target_id: replacement.target_id.to_string(),
                deployment_id: replacement.deployment_id.to_string(),
                target_scope: match replacement.target_scope {
                    TakeoverTargetScope::Global => "global",
                    TakeoverTargetScope::Project => "project",
                }
                .into(),
                path: Path::new(&replacement.target_root)
                    .join(replacement.target_relative_path.as_str())
                    .to_string_lossy()
                    .into_owned(),
                requested_mode: mode_dto(replacement.deployment_mode),
                resolved_mode: mode_dto(replacement.deployment_mode),
                fallback_reason: None,
            })
            .collect(),
        entry_count: u32::try_from(stats.entry_count).unwrap_or(u32::MAX),
        byte_count: u32::try_from(stats.regular_file_bytes).unwrap_or(u32::MAX),
        blockers: plan
            .content
            .blockers
            .iter()
            .map(|v| v.detail.clone())
            .collect(),
        recovery_summary: format!(
            "{} protected original(s)",
            plan.content.recovery.snapshot_count
        ),
        recovery_count: plan.content.recovery.snapshot_count,
        cross_volume_consequence: plan.content.non_atomic_consequences.first().cloned(),
        execution_allowed: plan.content.blockers.is_empty(),
    }
}

#[allow(clippy::too_many_arguments)]
fn fingerprint(
    adapter_id: crate::domain::AdapterId,
    expected_kind: EntryKind,
    raw_symlink_target: Option<String>,
    metadata: Option<MetadataFingerprint>,
    bundle_digest: Option<crate::domain::BundleDigest>,
    managed_skill_id: Option<SkillId>,
    managed_deployment_id: Option<DeploymentId>,
    captured_at: UtcTimestamp,
) -> PathFingerprint {
    PathFingerprint {
        expected_kind,
        raw_symlink_target,
        metadata,
        bundle_digest,
        bundle_subpath: None,
        resolved_bundle_digest: None,
        managed_skill_id,
        managed_deployment_id,
        captured_at,
        adapter_id,
    }
}
fn object_relative(
    digest: crate::domain::BundleDigest,
) -> Result<BundleRelativePath, crate::domain::NameError> {
    let h = hex::encode(digest.bytes());
    BundleRelativePath::parse(&format!(
        ".manager/objects/sha256-bundle-v1/{}/{}",
        &h[..2],
        &h[2..]
    ))
}
fn mode(value: DeploymentModeDto) -> DeploymentMode {
    match value {
        DeploymentModeDto::Symlink => DeploymentMode::Symlink,
        DeploymentModeDto::ManagedCopy => DeploymentMode::ManagedCopy,
    }
}
fn mode_dto(value: DeploymentMode) -> DeploymentModeDto {
    match value {
        DeploymentMode::Symlink => DeploymentModeDto::Symlink,
        DeploymentMode::ManagedCopy => DeploymentModeDto::ManagedCopy,
    }
}
fn decision(value: TakeoverDecisionDto) -> TakeoverDecision {
    match value {
        TakeoverDecisionDto::AddToVault => TakeoverDecision::AddToVault,
        TakeoverDecisionDto::AddAndManage => TakeoverDecision::AddAndManage,
    }
}
fn decision_dto(value: TakeoverDecision) -> TakeoverDecisionDto {
    match value {
        TakeoverDecision::AddToVault => TakeoverDecisionDto::AddToVault,
        TakeoverDecision::AddAndManage => TakeoverDecisionDto::AddAndManage,
    }
}
fn checked_target_scope(
    observation: &ObservationRecord,
) -> Result<TakeoverTargetScope, TakeoverError> {
    match (observation.scope.as_str(), observation.project_id) {
        ("global", None) => Ok(TakeoverTargetScope::Global),
        ("project", Some(_)) => Ok(TakeoverTargetScope::Project),
        _ => Err(TakeoverError::InvalidSelection(
            "selected observation has inconsistent target scope authority".into(),
        )),
    }
}

fn same_file_identity(left: MetadataFingerprint, right: MetadataFingerprint) -> bool {
    left.kind == right.kind && left.device_id == right.device_id && left.file_id == right.file_id
}

fn ensure_vault_target_disjoint(
    vault_root: &Path,
    target_root: &Path,
) -> Result<(), TakeoverError> {
    let vault = vault_root.canonicalize()?;
    let target = target_root.canonicalize()?;
    if target.starts_with(&vault) || vault.starts_with(&target) {
        return Err(TakeoverError::InvalidSelection(
            "replacement target and Vault must not contain one another".into(),
        ));
    }
    Ok(())
}
fn invalid_id(entity: &'static str, error: &impl ToString) -> TakeoverError {
    TakeoverError::InvalidId {
        entity,
        detail: error.to_string(),
    }
}
fn parse_observation_id(value: &str) -> Result<ObservationId, TakeoverError> {
    ObservationId::from_str(value).map_err(|e| invalid_id("Observation", &e))
}
fn parse_operation_id(value: &str) -> Result<OperationId, TakeoverError> {
    OperationId::from_str(value).map_err(|e| invalid_id("Operation", &e))
}

fn read_stable_text(
    root: &Path,
    relative: &BundleRelativePath,
) -> Result<(String, u64), TakeoverError> {
    let root_meta = fs::symlink_metadata(root)?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(TakeoverError::UnsafePreviewPath);
    }
    let canonical_root = root.canonicalize()?;
    let mut path = root.to_path_buf();
    for component in relative.as_str().split('/') {
        path.push(component);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(TakeoverError::UnsafePreviewPath);
        }
    }
    if !path.canonicalize()?.starts_with(&canonical_root) {
        return Err(TakeoverError::UnsafePreviewPath);
    }
    let before = fs::symlink_metadata(&path)?;
    let fingerprint = MetadataFingerprint::from_metadata(&before);
    if !before.is_file() {
        return Err(TakeoverError::UnsafePreviewPath);
    }
    if before.len() > PREVIEW_LIMIT {
        return Err(TakeoverError::PreviewTooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0));
    fs::File::open(&path)?
        .take(PREVIEW_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > PREVIEW_LIMIT {
        return Err(TakeoverError::PreviewTooLarge);
    }
    if MetadataFingerprint::from_metadata(&fs::symlink_metadata(&path)?) != fingerprint {
        return Err(TakeoverError::UnstablePreview);
    }
    let size = bytes.len() as u64;
    let content = String::from_utf8(bytes).map_err(|_| TakeoverError::PreviewNotUtf8)?;
    Ok((content, size))
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        domain::{AdapterId, DeploymentName, ProjectId, normalized_path_identity},
        operations::{OperationBoundary, OperationFailpoints},
        persistence::{ObservationRecord, ProjectRecord},
    };
    use tempfile::{TempDir, tempdir};

    struct FailTakeoverAt(TakeoverBoundary);

    impl TakeoverFailpoints for FailTakeoverAt {
        fn check(&self, boundary: TakeoverBoundary) -> Result<(), OperationHookError> {
            if boundary == self.0 {
                return Err(hook(format!("injected takeover failure at {boundary:?}")));
            }
            Ok(())
        }
    }

    struct TakeoverActionAt {
        boundary: TakeoverBoundary,
        action: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl TakeoverFailpoints for TakeoverActionAt {
        fn check(&self, boundary: TakeoverBoundary) -> Result<(), OperationHookError> {
            if boundary == self.boundary
                && let Some(action) = self.action.lock().unwrap().take()
            {
                action();
            }
            Ok(())
        }
    }

    struct FailOperationAt(Vec<OperationBoundary>);

    impl OperationFailpoints for FailOperationAt {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if self.0.contains(&boundary) {
                return Err(hook(format!("injected operation failure at {boundary:?}")));
            }
            Ok(())
        }
    }

    struct CrashAtTakeoverBoundary {
        boundary: OperationBoundary,
        marker: PathBuf,
    }

    impl OperationFailpoints for CrashAtTakeoverBoundary {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if boundary == self.boundary {
                crate::filesystem::durable::atomic_write(&self.marker, b"ready")
                    .map_err(|error| hook(error.to_string()))?;
                loop {
                    thread::sleep(Duration::from_secs(60));
                }
            }
            Ok(())
        }
    }

    struct Fixture {
        temporary: TempDir,
        service: TakeoverService,
        source: PathBuf,
        observation_id: ObservationId,
    }

    fn fixture(name: &str, body: &str) -> Fixture {
        let temporary = tempdir().unwrap();
        let vault_root = temporary.path().join("vault");
        let support = temporary.path().join("support");
        let external_root = temporary.path().join("external");
        let source = external_root.join(name);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), body).unwrap();
        let vault = Arc::new(
            OpenVault::open(&vault_root, &support, std::slice::from_ref(&external_root)).unwrap(),
        );
        let observation_id = ObservationId::generate();
        let adapter_id = AdapterId::new("takeover-test", 1).unwrap();
        let digest = hash_bundle(&source, BundleCaps::default()).unwrap().digest;
        let now = UtcTimestamp::now();
        vault
            .repositories
            .upsert_observation(ObservationRecord {
                id: observation_id,
                skill_id: None,
                adapter_id,
                scope: "global".into(),
                project_id: None,
                source_root_kind: "test".into(),
                source_root_id: "test-root".into(),
                display_path: source.clone(),
                normalized_path: normalized_path_identity(source.to_str().unwrap()),
                canonical_path: Some(source.canonicalize().unwrap()),
                deployment_name: DeploymentName::parse(name).unwrap(),
                digest: Some(digest),
                status: "verified".into(),
                error_code: None,
                error_summary: None,
                last_successful_run_id: None,
                first_seen_at: now,
                observed_at: now,
                stale_at: None,
            })
            .unwrap();
        Fixture {
            temporary,
            service: TakeoverService::new(vault),
            source,
            observation_id,
        }
    }

    fn add_observation(
        fixture: &Fixture,
        root_name: &str,
        name: &str,
        body: &str,
    ) -> (ObservationId, PathBuf) {
        let source = fixture.temporary.path().join(root_name).join(name);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), body).unwrap();
        let id = ObservationId::generate();
        let now = UtcTimestamp::now();
        fixture
            .service
            .vault
            .repositories
            .upsert_observation(ObservationRecord {
                id,
                skill_id: None,
                adapter_id: AdapterId::new("takeover-test", 1).unwrap(),
                scope: "global".into(),
                project_id: None,
                source_root_kind: "test".into(),
                source_root_id: root_name.into(),
                display_path: source.clone(),
                normalized_path: normalized_path_identity(source.to_str().unwrap()),
                canonical_path: Some(source.canonicalize().unwrap()),
                deployment_name: DeploymentName::parse(name).unwrap(),
                digest: Some(hash_bundle(&source, BundleCaps::default()).unwrap().digest),
                status: "verified".into(),
                error_code: None,
                error_summary: None,
                last_successful_run_id: None,
                first_seen_at: now,
                observed_at: now,
                stale_at: None,
            })
            .unwrap();
        (id, source)
    }

    fn persist_observation_at(
        fixture: &Fixture,
        path: &Path,
        name: &str,
        scope: &str,
        project_id: Option<ProjectId>,
        source_root_kind: &str,
    ) -> ObservationId {
        let id = ObservationId::generate();
        let now = UtcTimestamp::now();
        fixture
            .service
            .vault
            .repositories
            .upsert_observation(ObservationRecord {
                id,
                skill_id: None,
                adapter_id: AdapterId::new("takeover-test", 1).unwrap(),
                scope: scope.into(),
                project_id,
                source_root_kind: source_root_kind.into(),
                source_root_id: format!("{source_root_kind}-root"),
                display_path: path.to_path_buf(),
                normalized_path: normalized_path_identity(path.to_str().unwrap()),
                canonical_path: Some(path.canonicalize().unwrap()),
                deployment_name: DeploymentName::parse(name).unwrap(),
                digest: Some(hash_bundle(path, BundleCaps::default()).unwrap().digest),
                status: "verified".into(),
                error_code: None,
                error_summary: None,
                last_successful_run_id: None,
                first_seen_at: now,
                observed_at: now,
                stale_at: None,
            })
            .unwrap();
        id
    }

    fn metadata(path: &Path) -> MetadataFingerprint {
        MetadataFingerprint::from_metadata(&fs::symlink_metadata(path).unwrap())
    }

    #[test]
    fn preview_reader_enforces_containment_symlinks_and_utf8() {
        let temporary = tempdir().unwrap();
        fs::create_dir(temporary.path().join("nested")).unwrap();
        fs::write(temporary.path().join("nested/good.txt"), "hello").unwrap();
        assert_eq!(
            read_stable_text(
                temporary.path(),
                &BundleRelativePath::parse("nested/good.txt").unwrap()
            )
            .unwrap(),
            ("hello".into(), 5)
        );
        fs::write(temporary.path().join("bad.txt"), [0xff]).unwrap();
        assert!(matches!(
            read_stable_text(
                temporary.path(),
                &BundleRelativePath::parse("bad.txt").unwrap()
            ),
            Err(TakeoverError::PreviewNotUtf8)
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("nested", temporary.path().join("link")).unwrap();
            assert!(matches!(
                read_stable_text(
                    temporary.path(),
                    &BundleRelativePath::parse("link/good.txt").unwrap()
                ),
                Err(TakeoverError::UnsafePreviewPath)
            ));
        }
        assert!(BundleRelativePath::parse("../outside").is_err());
    }

    #[test]
    fn add_to_vault_executes_through_kernel_and_creates_no_deployment() {
        let fixture = fixture("example", "---\nname: example\n---\n");
        let source_before = hash_bundle(&fixture.source, BundleCaps::default()).unwrap();
        let source_root_before = metadata(&fixture.source);
        let source_manifest_before = metadata(&fixture.source.join("SKILL.md"));
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();

        let result = fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();

        assert_eq!(result.state, "Finalized");
        assert_eq!(result.review, plan);
        let source_after = hash_bundle(&fixture.source, BundleCaps::default()).unwrap();
        assert_eq!(source_after, source_before);
        assert_eq!(metadata(&fixture.source), source_root_before);
        assert_eq!(
            metadata(&fixture.source.join("SKILL.md")),
            source_manifest_before
        );
        let detail = fixture.service.skill_detail(&plan.skill_id).unwrap();
        assert_eq!(detail.ownership, "vaulted");
        assert!(detail.deployment_paths.is_empty());
        assert_eq!(detail.working_digest, plan.reviewed_digest);
        assert_eq!(
            fixture
                .service
                .vault
                .repositories
                .observation(fixture.observation_id)
                .unwrap()
                .unwrap()
                .skill_id,
            None,
            "copying an external source into the Vault must not claim its location"
        );
        let replay = fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();
        assert!(replay.replayed);
        let counts = fixture
            .service
            .vault
            .database
            .execute(|connection| {
                let activity =
                    connection.query_row("SELECT count(*) FROM activity", [], |row| {
                        row.get::<_, u32>(0)
                    })?;
                let revisions =
                    connection.query_row("SELECT count(*) FROM skill_revisions", [], |row| {
                        row.get::<_, u32>(0)
                    })?;
                Ok((activity, revisions))
            })
            .unwrap();
        assert_eq!(counts, (1, 1));
    }

    #[test]
    fn source_and_physical_alias_are_never_eligible_replacements() {
        let fixture = fixture("example", "source\n");
        let operations = fixture.service.vault.paths.manager().join("operations");
        let source_before = hash_bundle(&fixture.source, BundleCaps::default()).unwrap();
        let source_metadata = metadata(&fixture.source);

        let source_result = fixture.service.plan_takeover(TakeoverPlanRequest {
            source_observation_id: fixture.observation_id.to_string(),
            decision: TakeoverDecisionDto::AddAndManage,
            selected_locations: vec![SelectedLocationRequest {
                observation_id: fixture.observation_id.to_string(),
                mode: DeploymentModeDto::Symlink,
            }],
        });
        assert!(matches!(
            source_result,
            Err(TakeoverError::InvalidSelection(_))
        ));
        assert_eq!(fs::read_dir(&operations).unwrap().count(), 0);

        let alias_parent = fixture.source.parent().unwrap().join("alias-parent");
        fs::create_dir(&alias_parent).unwrap();
        let alias = alias_parent.join("..").join("example");
        let alias_id = persist_observation_at(&fixture, &alias, "example", "global", None, "alias");
        let alias_result = fixture.service.plan_takeover(TakeoverPlanRequest {
            source_observation_id: fixture.observation_id.to_string(),
            decision: TakeoverDecisionDto::AddAndManage,
            selected_locations: vec![SelectedLocationRequest {
                observation_id: alias_id.to_string(),
                mode: DeploymentModeDto::ManagedCopy,
            }],
        });
        assert!(matches!(
            alias_result,
            Err(TakeoverError::InvalidSelection(_))
        ));
        assert_eq!(fs::read_dir(&operations).unwrap().count(), 0);
        assert_eq!(
            hash_bundle(&fixture.source, BundleCaps::default()).unwrap(),
            source_before
        );
        assert_eq!(metadata(&fixture.source), source_metadata);
        assert_eq!(
            fs::read(fixture.source.join("SKILL.md")).unwrap(),
            b"source\n"
        );
    }

    #[test]
    fn replacement_target_and_vault_nesting_is_rejected_in_both_directions() {
        let fixture = fixture("example", "same\n");
        let operations = fixture.service.vault.paths.manager().join("operations");
        let inside = fixture
            .service
            .vault
            .paths
            .root()
            .join("nested-target/example");
        fs::create_dir_all(&inside).unwrap();
        fs::write(inside.join("SKILL.md"), b"same\n").unwrap();
        let inside_id =
            persist_observation_at(&fixture, &inside, "example", "global", None, "test");
        assert!(matches!(
            fixture.service.plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: inside_id.to_string(),
                    mode: DeploymentModeDto::Symlink,
                }],
            }),
            Err(TakeoverError::InvalidSelection(_))
        ));

        let contains = fixture.temporary.path().join("contains-vault");
        fs::create_dir(&contains).unwrap();
        fs::write(contains.join("SKILL.md"), b"same\n").unwrap();
        let contains_id = persist_observation_at(
            &fixture,
            &contains,
            "contains-vault",
            "global",
            None,
            "test",
        );
        assert!(matches!(
            fixture.service.plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: contains_id.to_string(),
                    mode: DeploymentModeDto::ManagedCopy,
                }],
            }),
            Err(TakeoverError::InvalidSelection(_))
        ));
        assert_eq!(fs::read_dir(&operations).unwrap().count(), 0);
        assert_eq!(
            fs::read(fixture.source.join("SKILL.md")).unwrap(),
            b"same\n"
        );
        assert_eq!(fs::read(inside.join("SKILL.md")).unwrap(), b"same\n");
        assert_eq!(fs::read(contains.join("SKILL.md")).unwrap(), b"same\n");
    }

    #[test]
    fn reused_target_authority_is_preserved_during_takeover_finalization() {
        let fixture = fixture("example", "same\n");
        let (selected_id, selected) =
            add_observation(&fixture, "selected-root", "example", "same\n");
        let target_root = selected.parent().unwrap().canonicalize().unwrap();
        let target_id = TargetId::generate();
        let created_at = UtcTimestamp::now();
        fixture
            .service
            .vault
            .repositories
            .upsert_target(TargetRecord {
                id: target_id,
                adapter_id: AdapterId::new("takeover-test", 1).unwrap(),
                scope: "global".into(),
                root_path: target_root.clone(),
                canonical_root_path: target_root.clone(),
                project_id: None,
                is_override: true,
                is_custom: true,
                created_at,
                updated_at: created_at,
            })
            .unwrap();
        let authority_before = fixture
            .service
            .vault
            .repositories
            .target_by_identity(
                AdapterId::new("takeover-test", 1).unwrap(),
                "global".into(),
                None,
                &target_root,
            )
            .unwrap()
            .unwrap();

        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::ManagedCopy,
                }],
            })
            .unwrap();
        assert_eq!(
            plan.selected_replacements[0].target_id,
            target_id.to_string()
        );
        fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();

        let persisted = fixture
            .service
            .vault
            .repositories
            .target_by_identity(
                AdapterId::new("takeover-test", 1).unwrap(),
                "global".into(),
                None,
                &target_root,
            )
            .unwrap()
            .unwrap();
        assert_eq!(persisted.id, target_id);
        assert_eq!(persisted.adapter_id, authority_before.adapter_id);
        assert_eq!(persisted.scope, authority_before.scope);
        assert_eq!(persisted.root_path, authority_before.root_path);
        assert_eq!(
            persisted.canonical_root_path,
            authority_before.canonical_root_path
        );
        assert_eq!(persisted.project_id, authority_before.project_id);
        assert_eq!(persisted.is_override, authority_before.is_override);
        assert_eq!(persisted.is_custom, authority_before.is_custom);
        assert_eq!(persisted.created_at, authority_before.created_at);
    }

    #[test]
    fn new_project_target_retains_the_observation_project_authority() {
        let fixture = fixture("example", "same\n");
        let project_root = fixture.temporary.path().join("project");
        let selected = project_root.join("example");
        fs::create_dir_all(&selected).unwrap();
        fs::write(selected.join("SKILL.md"), b"same\n").unwrap();
        let project_id = ProjectId::generate();
        let now = UtcTimestamp::now();
        fixture
            .service
            .vault
            .repositories
            .upsert_project(ProjectRecord {
                id: project_id,
                workspace_root_id: None,
                root_path: project_root.clone(),
                canonical_path: project_root.canonicalize().unwrap(),
                discovery_evidence: "test".into(),
                git_classification: "none".into(),
                manual: true,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let selected_id = persist_observation_at(
            &fixture,
            &selected,
            "example",
            "project",
            Some(project_id),
            "project",
        );
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::ManagedCopy,
                }],
            })
            .unwrap();
        fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();

        let canonical_root = project_root.canonicalize().unwrap();
        let target = fixture
            .service
            .vault
            .repositories
            .target_by_identity(
                AdapterId::new("takeover-test", 1).unwrap(),
                "project".into(),
                Some(project_id),
                &canonical_root,
            )
            .unwrap()
            .unwrap();
        assert_eq!(target.project_id, Some(project_id));
        assert_eq!(target.scope, "project");
        assert!(!target.is_override);
        assert!(!target.is_custom);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn target_lookup_selects_the_exact_project_stable_identity() {
        let fixture = fixture("example", "same\n");
        let adapter = AdapterId::new("takeover-test", 1).unwrap();
        let project_a = ProjectId::from_str("018f0000-0000-7000-8000-000000000201").unwrap();
        let project_b = ProjectId::from_str("018f0000-0000-7000-8000-000000000202").unwrap();
        let target_a = TargetId::from_str("018f0000-0000-7000-8000-000000000203").unwrap();
        let target_b = TargetId::from_str("018f0000-0000-7000-8000-000000000204").unwrap();
        let now = UtcTimestamp::now();
        for (id, name) in [(project_a, "project-a"), (project_b, "project-b")] {
            let root = fixture.temporary.path().join(name);
            fs::create_dir(&root).unwrap();
            fixture
                .service
                .vault
                .repositories
                .upsert_project(ProjectRecord {
                    id,
                    workspace_root_id: None,
                    root_path: root.clone(),
                    canonical_path: root.canonicalize().unwrap(),
                    discovery_evidence: "stable-identity-test".into(),
                    git_classification: "none".into(),
                    manual: true,
                    created_at: now,
                    updated_at: now,
                })
                .unwrap();
        }
        let shared_root = fixture.temporary.path().join("shared-target");
        let selected = shared_root.join("example");
        fs::create_dir_all(&selected).unwrap();
        fs::write(selected.join("SKILL.md"), b"same\n").unwrap();
        let canonical_root = shared_root.canonicalize().unwrap();
        for record in [
            TargetRecord {
                id: target_a,
                adapter_id: adapter.clone(),
                scope: "project".into(),
                root_path: canonical_root.clone(),
                canonical_root_path: canonical_root.clone(),
                project_id: Some(project_a),
                is_override: false,
                is_custom: true,
                created_at: now,
                updated_at: now,
            },
            TargetRecord {
                id: target_b,
                adapter_id: adapter.clone(),
                scope: "project".into(),
                root_path: canonical_root.clone(),
                canonical_root_path: canonical_root.clone(),
                project_id: Some(project_b),
                is_override: true,
                is_custom: false,
                created_at: now,
                updated_at: now,
            },
        ] {
            fixture
                .service
                .vault
                .repositories
                .upsert_target(record)
                .unwrap();
        }
        let selected_id = persist_observation_at(
            &fixture,
            &selected,
            "example",
            "project",
            Some(project_b),
            "project",
        );

        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::ManagedCopy,
                }],
            })
            .unwrap();
        assert_eq!(
            plan.selected_replacements[0].target_id,
            target_b.to_string()
        );
        fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();

        let authority_a = fixture
            .service
            .vault
            .repositories
            .target_by_identity(
                adapter.clone(),
                "project".into(),
                Some(project_a),
                &canonical_root,
            )
            .unwrap()
            .unwrap();
        let authority_b = fixture
            .service
            .vault
            .repositories
            .target_by_identity(adapter, "project".into(), Some(project_b), &canonical_root)
            .unwrap()
            .unwrap();
        assert_eq!(authority_a.id, target_a);
        assert_eq!(authority_a.project_id, Some(project_a));
        assert!(!authority_a.is_override);
        assert!(authority_a.is_custom);
        assert_eq!(authority_b.id, target_b);
        assert_eq!(authority_b.project_id, Some(project_b));
        assert!(authority_b.is_override);
        assert!(!authority_b.is_custom);
    }

    #[test]
    fn add_and_manage_replaces_only_the_explicit_same_digest_location() {
        let fixture = fixture("example", "skill\n");
        let (selected_id, selected) =
            add_observation(&fixture, "selected-root", "renamed-example", "skill\n");
        let (unselected_id, unselected) =
            add_observation(&fixture, "unselected-root", "example", "skill\n");
        let unselected_before = metadata(&unselected);
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::Symlink,
                }],
            })
            .unwrap();
        assert_eq!(plan.observations.len(), 3);
        assert_eq!(plan.selected_replacements.len(), 1);
        assert_eq!(plan.recovery_count, 1);

        fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();

        assert!(
            fs::symlink_metadata(&selected)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::metadata(&selected).unwrap().is_dir());
        assert_eq!(metadata(&unselected), unselected_before);
        assert_eq!(fs::read(unselected.join("SKILL.md")).unwrap(), b"skill\n");
        assert!(fs::symlink_metadata(&fixture.source).unwrap().is_dir());
        assert_eq!(
            fixture
                .service
                .vault
                .repositories
                .observation(fixture.observation_id)
                .unwrap()
                .unwrap()
                .skill_id,
            None
        );
        assert_eq!(
            fixture
                .service
                .vault
                .repositories
                .observation(unselected_id)
                .unwrap()
                .unwrap()
                .skill_id,
            None
        );
        assert_eq!(
            fixture
                .service
                .vault
                .repositories
                .observation(selected_id)
                .unwrap()
                .unwrap()
                .skill_id
                .map(|value| value.to_string()),
            Some(plan.skill_id.clone())
        );
        let detail = fixture.service.skill_detail(&plan.skill_id).unwrap();
        assert_eq!(detail.ownership, "managed");
        assert_eq!(
            detail.deployment_paths,
            vec![
                selected
                    .parent()
                    .unwrap()
                    .canonicalize()
                    .unwrap()
                    .join("renamed-example")
                    .to_string_lossy()
            ]
        );
        assert_eq!(detail.observation_paths.len(), 3);
        let stored = fixture.service.get_operation(&plan.operation_id).unwrap();
        assert_eq!(stored.recovery.len(), 1);
    }

    #[test]
    fn selected_managed_copy_uses_the_reviewed_vault_revision() {
        let fixture = fixture("example", "reviewed\n");
        let (selected_id, selected) =
            add_observation(&fixture, "selected-root", "example", "reviewed\n");
        let reviewed = hash_bundle(&fixture.source, BundleCaps::default())
            .unwrap()
            .digest;
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::ManagedCopy,
                }],
            })
            .unwrap();
        assert_eq!(
            plan.selected_replacements[0].resolved_mode,
            DeploymentModeDto::ManagedCopy
        );

        fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();

        assert!(fs::symlink_metadata(&selected).unwrap().is_dir());
        assert_eq!(
            hash_bundle(&selected, BundleCaps::default())
                .unwrap()
                .digest,
            reviewed
        );
        let detail = fixture.service.skill_detail(&plan.skill_id).unwrap();
        assert_eq!(detail.ownership, "managed");
        assert_eq!(detail.deployment_paths.len(), 1);
    }

    #[test]
    fn stale_reviewed_source_fails_without_working_activation() {
        let fixture = fixture("example", "before\n");
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();
        fs::write(fixture.source.join("SKILL.md"), "after\n").unwrap();

        assert!(matches!(
            fixture
                .service
                .execute_operation(&plan.operation_id, &plan.plan_digest),
            Err(TakeoverError::Operation(OperationError::StageFailed(_)))
        ));
        let operation = fixture.service.get_operation(&plan.operation_id).unwrap();
        assert_eq!(operation.state, "Failed");
        assert_eq!(operation.outcome.as_deref(), Some("FailedNoWrites"));
        assert!(
            !fixture
                .service
                .vault
                .paths
                .root()
                .join(&plan.working_path)
                .exists()
        );
    }

    #[test]
    fn changed_observation_authority_fails_closed_before_activation() {
        let fixture = fixture("example", "reviewed\n");
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();
        let observation = fixture
            .service
            .vault
            .repositories
            .observation(fixture.observation_id)
            .unwrap()
            .unwrap();
        fixture
            .service
            .vault
            .repositories
            .upsert_observation(ObservationRecord {
                source_root_id: "changed-root".into(),
                ..observation
            })
            .unwrap();

        assert!(matches!(
            fixture
                .service
                .execute_operation(&plan.operation_id, &plan.plan_digest),
            Err(TakeoverError::Operation(OperationError::StageFailed(_)))
        ));
        assert_eq!(
            fixture
                .service
                .get_operation(&plan.operation_id)
                .unwrap()
                .outcome
                .as_deref(),
            Some("FailedNoWrites")
        );
        assert!(
            !fixture
                .service
                .vault
                .paths
                .root()
                .join(&plan.working_path)
                .exists()
        );
    }

    #[test]
    fn same_named_skills_coexist_and_keep_external_creates_no_operation() {
        let fixture = fixture("example", "one\n");
        let (second_id, _) = add_observation(&fixture, "second-root", "example", "two\n");
        let operations = fixture.service.vault.paths.manager().join("operations");
        assert_eq!(fs::read_dir(&operations).unwrap().count(), 0);
        fixture
            .service
            .keep_external(&KeepExternalRequest {
                observation_id: fixture.observation_id.to_string(),
            })
            .unwrap();
        assert_eq!(fs::read_dir(&operations).unwrap().count(), 0);

        let first = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();
        fixture
            .service
            .execute_operation(&first.operation_id, &first.plan_digest)
            .unwrap();
        let second = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: second_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();
        fixture
            .service
            .execute_operation(&second.operation_id, &second.plan_digest)
            .unwrap();

        assert_ne!(first.skill_id, second.skill_id);
        assert_ne!(first.working_path, second.working_path);
        assert!(
            fixture
                .service
                .vault
                .paths
                .root()
                .join(first.working_path)
                .is_dir()
        );
        assert!(
            fixture
                .service
                .vault
                .paths
                .root()
                .join(second.working_path)
                .is_dir()
        );
    }

    #[test]
    fn unsafe_symlink_and_caps_fail_before_plan_persistence() {
        let fixture = fixture("example", "skill\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing", fixture.source.join("broken")).unwrap();
        let operation_count =
            fs::read_dir(fixture.service.vault.paths.manager().join("operations"))
                .unwrap()
                .count();
        assert!(
            fixture
                .service
                .plan_takeover(TakeoverPlanRequest {
                    source_observation_id: fixture.observation_id.to_string(),
                    decision: TakeoverDecisionDto::AddToVault,
                    selected_locations: Vec::new(),
                })
                .is_err()
        );
        assert_eq!(
            fs::read_dir(fixture.service.vault.paths.manager().join("operations"))
                .unwrap()
                .count(),
            operation_count
        );

        fs::remove_file(fixture.source.join("broken")).unwrap();
        fs::write(fixture.source.join("extra"), b"x").unwrap();
        let observation = fixture
            .service
            .vault
            .repositories
            .observation(fixture.observation_id)
            .unwrap()
            .unwrap();
        assert!(
            inspect_observation(
                &ObservationRecord {
                    digest: Some(
                        hash_bundle(&fixture.source, BundleCaps::default())
                            .unwrap()
                            .digest
                    ),
                    ..observation
                },
                BundleCaps {
                    maximum_entries: 1,
                    ..BundleCaps::default()
                }
            )
            .is_err()
        );

        #[cfg(unix)]
        {
            use std::{ffi::OsString, os::unix::ffi::OsStringExt};

            let unsupported = fixture
                .temporary
                .path()
                .join(OsString::from_vec(vec![b's', b'k', b'i', b'l', b'l', 0xff]));
            let observation = fixture
                .service
                .vault
                .repositories
                .observation(fixture.observation_id)
                .unwrap()
                .unwrap();
            assert!(matches!(
                inspect_observation(
                    &ObservationRecord {
                        display_path: unsupported,
                        canonical_path: None,
                        ..observation
                    },
                    BundleCaps::default()
                ),
                Err(TakeoverError::ObservationNotExternal)
            ));
        }
    }

    #[test]
    fn takeover_validation_hash_and_object_failpoints_leave_no_active_skill() {
        for boundary in [
            TakeoverBoundary::SourceValidated,
            TakeoverBoundary::SourceHashed,
            TakeoverBoundary::ObjectPublished,
        ] {
            let fixture = fixture("example", "reviewed\n");
            let source_before = hash_bundle(&fixture.source, BundleCaps::default()).unwrap();
            let plan = fixture
                .service
                .plan_takeover(TakeoverPlanRequest {
                    source_observation_id: fixture.observation_id.to_string(),
                    decision: TakeoverDecisionDto::AddToVault,
                    selected_locations: Vec::new(),
                })
                .unwrap();
            let service = TakeoverService::new(Arc::clone(&fixture.service.vault)).with_failpoints(
                Arc::new(FailTakeoverAt(boundary)),
                Arc::new(crate::operations::NoopOperationFailpoints),
            );

            assert!(matches!(
                service.execute_operation(&plan.operation_id, &plan.plan_digest),
                Err(TakeoverError::Operation(OperationError::StageFailed(_)))
            ));
            let operation = service.get_operation(&plan.operation_id).unwrap();
            assert_eq!(operation.state, "Failed");
            assert_eq!(operation.outcome.as_deref(), Some("FailedNoWrites"));
            assert!(!service.vault.paths.root().join(&plan.working_path).exists());
            assert_eq!(
                hash_bundle(&fixture.source, BundleCaps::default()).unwrap(),
                source_before
            );
            assert!(
                service
                    .vault
                    .repositories
                    .skill(SkillId::from_str(&plan.skill_id).unwrap())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn source_changed_during_object_copy_is_rejected_before_activation() {
        let fixture = fixture("example", "reviewed\n");
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();
        let source = fixture.source.clone();
        let service = TakeoverService::new(Arc::clone(&fixture.service.vault)).with_failpoints(
            Arc::new(TakeoverActionAt {
                boundary: TakeoverBoundary::ObjectPublished,
                action: Mutex::new(Some(Box::new(move || {
                    fs::write(source.join("SKILL.md"), "changed concurrently\n").unwrap();
                }))),
            }),
            Arc::new(crate::operations::NoopOperationFailpoints),
        );

        assert!(matches!(
            service.execute_operation(&plan.operation_id, &plan.plan_digest),
            Err(TakeoverError::Operation(OperationError::StageFailed(_)))
        ));
        assert_eq!(
            service
                .get_operation(&plan.operation_id)
                .unwrap()
                .outcome
                .as_deref(),
            Some("FailedNoWrites")
        );
        assert!(!service.vault.paths.root().join(&plan.working_path).exists());
    }

    #[test]
    fn activation_and_selected_replacement_failpoints_restore_every_active_path() {
        let first_fixture = fixture("example", "reviewed\n");
        let plan = first_fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: first_fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();
        let service = TakeoverService::new(Arc::clone(&first_fixture.service.vault))
            .with_failpoints(
                Arc::new(NoopTakeoverFailpoints),
                Arc::new(FailOperationAt(vec![OperationBoundary::FinalRenamed(0)])),
            );
        assert!(matches!(
            service.execute_operation(&plan.operation_id, &plan.plan_digest),
            Err(TakeoverError::Operation(
                OperationError::ExecutionFailedRolledBack(_)
            ))
        ));
        assert!(!service.vault.paths.root().join(&plan.working_path).exists());
        assert_eq!(
            service
                .get_operation(&plan.operation_id)
                .unwrap()
                .outcome
                .as_deref(),
            Some("FailedRolledBack")
        );

        let fixture = fixture("example", "reviewed\n");
        let (selected_id, selected) =
            add_observation(&fixture, "selected-root", "example", "reviewed\n");
        let selected_before = hash_bundle(&selected, BundleCaps::default()).unwrap();
        let selected_metadata = metadata(&selected);
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::Symlink,
                }],
            })
            .unwrap();
        let service = TakeoverService::new(Arc::clone(&fixture.service.vault)).with_failpoints(
            Arc::new(NoopTakeoverFailpoints),
            Arc::new(FailOperationAt(vec![OperationBoundary::FinalRenamed(1)])),
        );
        assert!(matches!(
            service.execute_operation(&plan.operation_id, &plan.plan_digest),
            Err(TakeoverError::Operation(
                OperationError::ExecutionFailedRolledBack(_)
            ))
        ));
        assert_eq!(
            hash_bundle(&selected, BundleCaps::default()).unwrap(),
            selected_before
        );
        assert_eq!(metadata(&selected), selected_metadata);
        assert!(!service.vault.paths.root().join(&plan.working_path).exists());
        let operation = service.get_operation(&plan.operation_id).unwrap();
        assert_eq!(operation.outcome.as_deref(), Some("FailedRolledBack"));
        assert_eq!(operation.recovery.len(), 1);
    }

    #[test]
    fn manifest_and_projection_failpoints_leave_committed_recoverable_evidence() {
        for boundary in [
            OperationBoundary::ManifestsPublished,
            OperationBoundary::ProjectionFinalized,
        ] {
            let fixture = fixture("example", "reviewed\n");
            let plan = fixture
                .service
                .plan_takeover(TakeoverPlanRequest {
                    source_observation_id: fixture.observation_id.to_string(),
                    decision: TakeoverDecisionDto::AddToVault,
                    selected_locations: Vec::new(),
                })
                .unwrap();
            let service = TakeoverService::new(Arc::clone(&fixture.service.vault)).with_failpoints(
                Arc::new(NoopTakeoverFailpoints),
                Arc::new(FailOperationAt(vec![boundary])),
            );

            assert!(matches!(
                service.execute_operation(&plan.operation_id, &plan.plan_digest),
                Err(TakeoverError::Operation(
                    OperationError::FinalizationInterrupted(_)
                ))
            ));
            assert!(service.vault.paths.root().join(&plan.working_path).is_dir());
            let operation = service.get_operation(&plan.operation_id).unwrap();
            assert_eq!(operation.state, "Committed");
            assert_eq!(operation.outcome, None);
            let store = OperationStore::open(service.vault.paths.manager()).unwrap();
            let stored = store
                .load(OperationId::from_str(&plan.operation_id).unwrap())
                .unwrap();
            let mut roots = TargetRoots::new();
            roots.insert(
                stored
                    .plan
                    .content
                    .takeover
                    .as_ref()
                    .unwrap()
                    .skill
                    .working_target_id,
                AuthorizedRoot::open(service.vault.paths.root()).unwrap(),
            );
            assert_eq!(
                crate::operations::classify_startup(&stored, &roots).unwrap(),
                crate::operations::StartupDecision::ContinueFinalization
            );
        }
    }

    #[test]
    fn rollback_durability_failure_preserves_takeover_evidence_for_recovery() {
        let fixture = fixture("example", "reviewed\n");
        let (selected_id, selected) =
            add_observation(&fixture, "selected-root", "example", "reviewed\n");
        let selected_before = hash_bundle(&selected, BundleCaps::default()).unwrap();
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::Symlink,
                }],
            })
            .unwrap();
        let service = TakeoverService::new(Arc::clone(&fixture.service.vault)).with_failpoints(
            Arc::new(NoopTakeoverFailpoints),
            Arc::new(FailOperationAt(vec![
                OperationBoundary::FinalRenamed(1),
                OperationBoundary::RollbackObserved(1),
            ])),
        );

        assert!(matches!(
            service.execute_operation(&plan.operation_id, &plan.plan_digest),
            Err(TakeoverError::Operation(OperationError::RecoveryRequired(
                _
            )))
        ));
        assert_eq!(
            hash_bundle(&selected, BundleCaps::default()).unwrap(),
            selected_before,
            "the selected original is physically restored before rollback evidence is sealed"
        );
        let operation = service.get_operation(&plan.operation_id).unwrap();
        assert_eq!(operation.state, "RecoveryRequired");
        assert_eq!(operation.outcome.as_deref(), Some("RecoveryRequired"));
        assert_eq!(operation.recovery.len(), 1);
        assert!(service.vault.paths.root().join(&plan.working_path).is_dir());
        assert!(
            service
                .vault
                .paths
                .root()
                .join(&plan.baseline_object_path)
                .is_dir()
        );
    }

    #[test]
    #[ignore = "invoked only by child_process_kill_reopens_takeover_evidence"]
    fn takeover_crash_child_helper() {
        let Ok(vault_root) = std::env::var("SKILLS_HUB_TAKEOVER_CHILD_VAULT") else {
            return;
        };
        let support = PathBuf::from(
            std::env::var("SKILLS_HUB_TAKEOVER_CHILD_SUPPORT").expect("child support path"),
        );
        let marker = PathBuf::from(
            std::env::var("SKILLS_HUB_TAKEOVER_CHILD_MARKER").expect("child marker path"),
        );
        let operation_id =
            std::env::var("SKILLS_HUB_TAKEOVER_CHILD_OPERATION").expect("child operation ID");
        let plan_digest =
            std::env::var("SKILLS_HUB_TAKEOVER_CHILD_DIGEST").expect("child plan digest");
        let vault = Arc::new(
            OpenVault::open(Path::new(&vault_root), &support, &[]).expect("child opens Vault"),
        );
        let service = TakeoverService::new(vault).with_failpoints(
            Arc::new(NoopTakeoverFailpoints),
            Arc::new(CrashAtTakeoverBoundary {
                boundary: OperationBoundary::FinalRenamed(1),
                marker,
            }),
        );
        let _ = service.execute_operation(&operation_id, &plan_digest);
        panic!("child takeover execution returned before parent killed it");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn child_process_kill_reopens_takeover_evidence_and_classifies_without_writes() {
        let fixture = fixture("example", "reviewed\n");
        let (selected_id, selected) =
            add_observation(&fixture, "selected-root", "example", "reviewed\n");
        let plan = fixture
            .service
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: fixture.observation_id.to_string(),
                decision: TakeoverDecisionDto::AddAndManage,
                selected_locations: vec![SelectedLocationRequest {
                    observation_id: selected_id.to_string(),
                    mode: DeploymentModeDto::Symlink,
                }],
            })
            .unwrap();
        let temporary = fixture.temporary;
        let source = fixture.source;
        let vault_root = fixture.service.vault.paths.root().to_path_buf();
        let support = temporary.path().join("support");
        drop(fixture.service);

        let marker = temporary.path().join("takeover-crash-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "application::takeover::tests::takeover_crash_child_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("SKILLS_HUB_TAKEOVER_CHILD_VAULT", &vault_root)
            .env("SKILLS_HUB_TAKEOVER_CHILD_SUPPORT", &support)
            .env("SKILLS_HUB_TAKEOVER_CHILD_MARKER", &marker)
            .env("SKILLS_HUB_TAKEOVER_CHILD_OPERATION", &plan.operation_id)
            .env("SKILLS_HUB_TAKEOVER_CHILD_DIGEST", &plan.plan_digest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not reach the durable takeover rename boundary"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            thread::sleep(Duration::from_millis(20));
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());

        let vault = Arc::new(OpenVault::open(&vault_root, &support, &[]).unwrap());
        let store = OperationStore::open(vault.paths.manager()).unwrap();
        let operation_id = OperationId::from_str(&plan.operation_id).unwrap();
        let stored = store.load(operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Committing);
        let context = stored.plan.content.takeover.as_ref().unwrap();
        let mut roots = TargetRoots::new();
        roots.insert(
            context.skill.working_target_id,
            AuthorizedRoot::open(vault.paths.root()).unwrap(),
        );
        for replacement in &context.replacements {
            roots.insert(
                replacement.target_id,
                AuthorizedRoot::open(Path::new(&replacement.target_root)).unwrap(),
            );
        }
        let before = (
            hash_bundle(&source, BundleCaps::default()).unwrap(),
            fs::read_link(&selected).unwrap(),
            hash_bundle(
                &vault
                    .paths
                    .root()
                    .join(context.skill.working_bundle_path.as_str()),
                BundleCaps::default(),
            )
            .unwrap(),
            fs::read(store.operation_directory(operation_id).join("journal.json")).unwrap(),
        );
        assert_eq!(
            crate::operations::classify_startup(&stored, &roots).unwrap(),
            crate::operations::StartupDecision::ContinueVerification
        );
        assert_eq!(
            crate::operations::classify_startup(&stored, &roots).unwrap(),
            crate::operations::StartupDecision::ContinueVerification
        );
        let after = (
            hash_bundle(&source, BundleCaps::default()).unwrap(),
            fs::read_link(&selected).unwrap(),
            hash_bundle(
                &vault
                    .paths
                    .root()
                    .join(context.skill.working_bundle_path.as_str()),
                BundleCaps::default(),
            )
            .unwrap(),
            fs::read(store.operation_directory(operation_id).join("journal.json")).unwrap(),
        );
        assert_eq!(
            after, before,
            "startup classification must be read-only and idempotent"
        );
    }
}
