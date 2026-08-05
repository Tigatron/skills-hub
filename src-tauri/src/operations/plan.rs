use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    domain::{
        ActivityId, AdapterId, BundleDigest, BundleRelativePath, DeploymentHealth, DeploymentId,
        DeploymentMode, DeploymentName, ObservationId, OperationId, ProjectId, SkillId,
        SkillLifecycle, SnapshotId, TargetId, TrashEntryId, UtcTimestamp,
    },
    filesystem::{
        AuthorizedPath, BundleCaps, BundleStats, EntryKind, MetadataFingerprint, PathIdentity,
    },
};

const PLAN_SCHEMA_VERSION_V1: u16 = 1;
const PLAN_SCHEMA_VERSION_V2: u16 = 2;
const PLAN_SCHEMA_VERSION_V3: u16 = 3;
const PLAN_SCHEMA_VERSION_V4: u16 = 4;
const PLAN_SCHEMA_VERSION_V5: u16 = 5;
const PLAN_HASH_DOMAIN_V1: &[u8] = b"skills-hub-operation-plan\0v1\0";
const PLAN_HASH_DOMAIN_V2: &[u8] = b"skills-hub-operation-plan\0v2\0";
const PLAN_HASH_DOMAIN_V3: &[u8] = b"skills-hub-operation-plan\0v3\0";
const PLAN_HASH_DOMAIN_V4: &[u8] = b"skills-hub-operation-plan\0v4\0";
const PLAN_HASH_DOMAIN_V5: &[u8] = b"skills-hub-operation-plan\0v5\0";
const PLAN_DIGEST_PREFIX_V1: &str = "sha256-operation-plan-v1:";
const PLAN_DIGEST_PREFIX_V2: &str = "sha256-operation-plan-v2:";
const PLAN_DIGEST_PREFIX_V3: &str = "sha256-operation-plan-v3:";
const PLAN_DIGEST_PREFIX_V4: &str = "sha256-operation-plan-v4:";
const PLAN_DIGEST_PREFIX_V5: &str = "sha256-operation-plan-v5:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationPlan {
    pub plan_digest: PlanDigest,
    #[serde(flatten)]
    pub content: OperationPlanContent,
}

impl OperationPlan {
    /// Validates immutable plan content and binds it to a canonical digest.
    ///
    /// # Errors
    ///
    /// Returns [`PlanBuildError`] for an invalid schema, expiry, step list, or serialization.
    #[allow(clippy::too_many_lines)]
    pub fn build(mut content: OperationPlanContent) -> Result<Self, PlanBuildError> {
        let (takeover_context, deployment_context, batch_context, trash_context) = match (
            content.schema_version,
            content.kind,
            content.takeover.as_ref(),
            content.deployment.as_ref(),
            content.batch_deployment.as_ref(),
            content.trash.as_ref(),
        ) {
            (PLAN_SCHEMA_VERSION_V1, _, None, None, None, None) => (None, None, None, None),
            (PLAN_SCHEMA_VERSION_V2, OperationKind::TakeOver, Some(evidence), None, None, None) => {
                (Some(evidence), None, None, None)
            }
            (
                PLAN_SCHEMA_VERSION_V3,
                OperationKind::Deploy | OperationKind::Undeploy,
                None,
                Some(evidence),
                None,
                None,
            ) => (None, Some(evidence), None, None),
            (
                PLAN_SCHEMA_VERSION_V4,
                OperationKind::Deploy | OperationKind::Undo,
                None,
                None,
                Some(evidence),
                None,
            ) => (None, None, Some(evidence), None),
            (
                PLAN_SCHEMA_VERSION_V5,
                OperationKind::MoveToTrash
                | OperationKind::Restore
                | OperationKind::PermanentlyDelete,
                None,
                None,
                None,
                Some(evidence),
            ) => (None, None, None, Some(evidence)),
            (PLAN_SCHEMA_VERSION_V1 | PLAN_SCHEMA_VERSION_V2, _, _, _, _, _) => {
                return Err(PlanBuildError::InvalidTakeoverContext);
            }
            (PLAN_SCHEMA_VERSION_V3, _, _, _, _, _) => {
                return Err(PlanBuildError::InvalidDeploymentContext);
            }
            (PLAN_SCHEMA_VERSION_V4, _, _, _, _, _) => {
                return Err(PlanBuildError::InvalidBatchContext);
            }
            (PLAN_SCHEMA_VERSION_V5, _, _, _, _, _) => {
                return Err(PlanBuildError::InvalidTrashContext);
            }
            _ => return Err(PlanBuildError::UnsupportedSchema(content.schema_version)),
        };
        if content.expires_at <= content.created_at {
            return Err(PlanBuildError::InvalidExpiry);
        }
        if content.steps.is_empty() {
            return Err(PlanBuildError::NoSteps);
        }
        for (index, step) in content.steps.iter_mut().enumerate() {
            step.order = u32::try_from(index).map_err(|_| PlanBuildError::TooManySteps)?;
        }
        if let Some(context) = takeover_context {
            validate_takeover(&content, context)?;
        }
        if let Some(context) = deployment_context {
            validate_deployment(&content, context)?;
        }
        if let Some(context) = batch_context {
            validate_batch_deployment(&content, context)?;
        }
        if let Some(context) = trash_context {
            validate_trash(&content, context)?;
        }
        validate_steps(content.schema_version, &content.steps)?;
        if content.steps.iter().any(PlanStep::is_destructive)
            && content.recovery.snapshot_count == 0
        {
            return Err(PlanBuildError::MissingRecoveryPoint);
        }
        content.selected_skill_ids.sort_unstable();
        content.selected_skill_ids.dedup();
        content.selected_target_ids.sort_unstable();
        content.selected_target_ids.dedup();
        content.selected_deployment_ids.sort_unstable();
        content.selected_deployment_ids.dedup();
        content.ownership_choices.sort_unstable();
        if content
            .ownership_choices
            .windows(2)
            .any(|choices| choices[0].skill_id == choices[1].skill_id)
        {
            return Err(PlanBuildError::DuplicateOwnershipChoice);
        }
        content.blockers.sort_unstable();
        content.blockers.dedup();
        content.non_atomic_consequences.sort_unstable();
        content.non_atomic_consequences.dedup();

        let canonical = serde_json::to_vec(&content).map_err(PlanBuildError::Serialize)?;
        let mut hasher = Sha256::new();
        hasher.update(match content.schema_version {
            PLAN_SCHEMA_VERSION_V1 => PLAN_HASH_DOMAIN_V1,
            PLAN_SCHEMA_VERSION_V2 => PLAN_HASH_DOMAIN_V2,
            PLAN_SCHEMA_VERSION_V3 => PLAN_HASH_DOMAIN_V3,
            PLAN_SCHEMA_VERSION_V4 => PLAN_HASH_DOMAIN_V4,
            PLAN_SCHEMA_VERSION_V5 => PLAN_HASH_DOMAIN_V5,
            _ => unreachable!("schema version was validated above"),
        });
        hasher.update(canonical);
        Ok(Self {
            plan_digest: PlanDigest {
                schema_version: content.schema_version,
                bytes: hasher.finalize().into(),
            },
            content,
        })
    }

    /// Serializes this reviewed plan to stable compact JSON.
    ///
    /// # Errors
    ///
    /// Returns [`PlanBuildError`] if serialization fails.
    pub fn canonical_json(&self) -> Result<Vec<u8>, PlanBuildError> {
        serde_json::to_vec(self).map_err(PlanBuildError::Serialize)
    }

    /// Recomputes and verifies this plan's confirmation digest.
    ///
    /// # Errors
    ///
    /// Returns [`PlanBuildError`] if content is invalid or its digest differs.
    pub fn verify_digest(&self) -> Result<(), PlanBuildError> {
        let rebuilt = Self::build(self.content.clone())?;
        if rebuilt.plan_digest == self.plan_digest && rebuilt.content == self.content {
            Ok(())
        } else {
            Err(PlanBuildError::DigestMismatch)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationPlanContent {
    pub schema_version: u16,
    pub operation_id: OperationId,
    pub kind: OperationKind,
    pub created_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub selected_skill_ids: Vec<SkillId>,
    pub selected_target_ids: Vec<TargetId>,
    pub selected_deployment_ids: Vec<DeploymentId>,
    pub ownership_choices: Vec<OwnershipChoice>,
    pub bundle_caps: BundleCaps,
    pub observed_bundle_stats: BundleStats,
    pub steps: Vec<PlanStep>,
    pub blockers: Vec<PlanBlocker>,
    pub recovery: RecoverySummary,
    pub non_atomic_consequences: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub takeover: Option<TakeoverPlanContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentPlanContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_deployment: Option<BatchDeploymentPlanContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash: Option<TrashPlanContext>,
}

impl OperationPlanContent {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        kind: OperationKind,
        created_at: UtcTimestamp,
        expires_at: UtcTimestamp,
        selected_skill_ids: Vec<SkillId>,
        selected_target_ids: Vec<TargetId>,
        selected_deployment_ids: Vec<DeploymentId>,
        ownership_choices: Vec<OwnershipChoice>,
        bundle_caps: BundleCaps,
        observed_bundle_stats: BundleStats,
        steps: Vec<PlanStep>,
        blockers: Vec<PlanBlocker>,
        recovery: RecoverySummary,
        non_atomic_consequences: Vec<String>,
    ) -> Self {
        Self {
            schema_version: PLAN_SCHEMA_VERSION_V1,
            operation_id,
            kind,
            created_at,
            expires_at,
            selected_skill_ids,
            selected_target_ids,
            selected_deployment_ids,
            ownership_choices,
            bundle_caps,
            observed_bundle_stats,
            steps,
            blockers,
            recovery,
            non_atomic_consequences,
            takeover: None,
            deployment: None,
            batch_deployment: None,
            trash: None,
        }
    }

    #[must_use]
    pub fn with_takeover_context(mut self, context: TakeoverPlanContext) -> Self {
        self.schema_version = PLAN_SCHEMA_VERSION_V2;
        self.takeover = Some(context);
        self
    }

    #[must_use]
    pub fn with_deployment_context(mut self, context: DeploymentPlanContext) -> Self {
        self.schema_version = PLAN_SCHEMA_VERSION_V3;
        self.deployment = Some(context);
        self
    }

    #[must_use]
    pub fn with_batch_deployment_context(mut self, context: BatchDeploymentPlanContext) -> Self {
        self.schema_version = PLAN_SCHEMA_VERSION_V4;
        self.batch_deployment = Some(context);
        self
    }

    #[must_use]
    pub fn with_trash_context(mut self, context: TrashPlanContext) -> Self {
        self.schema_version = PLAN_SCHEMA_VERSION_V5;
        self.trash = Some(context);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverDecision {
    AddToVault,
    AddAndManage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverObservationStatus {
    Present,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeoverTargetScope {
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverObservationEvidence {
    pub observation_id: ObservationId,
    pub skill_id: Option<SkillId>,
    pub adapter_id: AdapterId,
    pub target_scope: TakeoverTargetScope,
    pub project_id: Option<ProjectId>,
    pub source_root_kind: String,
    pub source_root_id: String,
    pub display_path: String,
    pub canonical_path: Option<String>,
    pub deployment_name: DeploymentName,
    pub bundle_digest: Option<BundleDigest>,
    pub status: TakeoverObservationStatus,
    pub error_code: Option<String>,
    pub error_summary: Option<String>,
    pub observed_at: UtcTimestamp,
    pub entry_kind: EntryKind,
    pub metadata: Option<MetadataFingerprint>,
    pub raw_symlink_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverSkillEvidence {
    pub skill_id: SkillId,
    pub display_name: String,
    pub deployment_name: DeploymentName,
    pub vault_root: String,
    pub working_target_id: TargetId,
    pub working_container_path: BundleRelativePath,
    pub working_bundle_path: BundleRelativePath,
    pub manifest_path: BundleRelativePath,
    pub baseline_digest: Option<BundleDigest>,
    pub baseline_object_path: Option<BundleRelativePath>,
    pub working_step_order: u32,
    pub activity_id: ActivityId,
    pub snapshot_id: Option<SnapshotId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverReplacementEvidence {
    pub observation_id: ObservationId,
    pub target_id: TargetId,
    pub deployment_id: DeploymentId,
    pub adapter_id: AdapterId,
    pub target_scope: TakeoverTargetScope,
    pub target_root: String,
    pub target_canonical_root: String,
    pub project_id: Option<ProjectId>,
    pub is_override: bool,
    pub is_custom: bool,
    pub existing_target: bool,
    pub target_relative_path: BundleRelativePath,
    pub deployment_mode: DeploymentMode,
    pub step_order: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverPlanContext {
    pub decision: TakeoverDecision,
    pub source_observation_id: ObservationId,
    pub observations: Vec<TakeoverObservationEvidence>,
    pub skill: TakeoverSkillEvidence,
    pub replacements: Vec<TakeoverReplacementEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentProductAction {
    Deploy,
    Undeploy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetCapabilityEvidence {
    pub directory_write: CapabilityStatus,
    pub atomic_rename: CapabilityStatus,
    pub symlink: CapabilityStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndeployResolution {
    RemoveManaged,
    PreserveTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentSkillEvidence {
    pub skill_id: SkillId,
    pub deployment_name: DeploymentName,
    pub vault_root: String,
    pub working_bundle_path: BundleRelativePath,
    pub reviewed_digest: BundleDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentTargetEvidence {
    pub target_id: TargetId,
    pub adapter_id: AdapterId,
    pub target_scope: TakeoverTargetScope,
    pub target_root: String,
    pub target_canonical_root: String,
    pub project_id: Option<ProjectId>,
    pub project_git_classification: Option<String>,
    pub is_override: bool,
    pub is_custom: bool,
    pub capability: TargetCapabilityEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedDeploymentEvidence {
    pub deployment_id: DeploymentId,
    pub deployment_created_at: UtcTimestamp,
    pub deployment_updated_at: UtcTimestamp,
    pub existing_deployment: bool,
    pub active_before: bool,
    pub target_relative_path: BundleRelativePath,
    pub requested_mode: DeploymentMode,
    pub resolved_mode: DeploymentMode,
    pub fallback_reason: Option<String>,
    pub previous_expected_digest: Option<BundleDigest>,
    pub previous_expected_link_target: Option<String>,
    pub reviewed_health: DeploymentHealth,
    pub resolution: Option<UndeployResolution>,
    pub step_order: u32,
    pub manifest_path: BundleRelativePath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentPlanContext {
    pub action: DeploymentProductAction,
    pub skill: DeploymentSkillEvidence,
    pub target: DeploymentTargetEvidence,
    pub deployment: ManagedDeploymentEvidence,
    pub activity_id: ActivityId,
    pub snapshot_id: Option<SnapshotId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchDeploymentAction {
    Deploy,
    Undo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeploymentEntryEvidence {
    pub target: DeploymentTargetEvidence,
    pub deployment: ManagedDeploymentEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverse: Option<BatchDeploymentInverseEvidence>,
}

/// Seals an inverse batch entry to the exact source step and protected before-version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeploymentInverseEvidence {
    pub source_operation_id: OperationId,
    pub source_step_order: u32,
    pub protected_reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeploymentPlanContext {
    pub action: BatchDeploymentAction,
    pub skill: DeploymentSkillEvidence,
    pub entries: Vec<BatchDeploymentEntryEvidence>,
    pub activity_id: ActivityId,
    pub snapshot_id: Option<SnapshotId>,
    pub undo_of: Option<OperationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrashAction {
    MoveToTrash,
    Restore,
    PermanentlyDelete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrashRetentionPolicy {
    Days30,
    Never,
}

/// Exact domain and filesystem evidence reviewed for a Trash transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrashPlanContext {
    pub action: TrashAction,
    pub skill_id: SkillId,
    pub display_name: String,
    pub deployment_name: DeploymentName,
    pub lifecycle_before: SkillLifecycle,
    pub lifecycle_after: SkillLifecycle,
    pub trash_entry_id: TrashEntryId,
    pub source_relative_path: BundleRelativePath,
    pub destination_relative_path: Option<BundleRelativePath>,
    pub skill_manifest_path: BundleRelativePath,
    pub provenance_paths: Vec<BundleRelativePath>,
    pub working_digest: BundleDigest,
    pub baseline_digest: BundleDigest,
    pub active_deployment_ids: Vec<DeploymentId>,
    pub deployments_resolved: bool,
    pub retention_policy: TrashRetentionPolicy,
    pub retention_deadline: Option<UtcTimestamp>,
    pub confirmation_subject: String,
    pub protected_reference_ids: Vec<String>,
    pub source_step_order: u32,
    pub destination_step_order: Option<u32>,
    pub snapshot_id: Option<SnapshotId>,
    pub activity_id: ActivityId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    TakeOver,
    Deploy,
    Undeploy,
    MoveToTrash,
    Restore,
    PermanentlyDelete,
    Undo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipChoice {
    pub skill_id: SkillId,
    pub decision: OwnershipDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipDecision {
    KeepExternal,
    TakeOver,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub order: u32,
    pub action: PlanAction,
    pub path: PlanPath,
    pub requested_mode: Option<DeploymentMode>,
    pub resolved_mode: Option<DeploymentMode>,
    pub before: PathFingerprint,
    pub after: PathFingerprint,
    pub recovery_required: bool,
}

impl PlanStep {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        action: PlanAction,
        path: PlanPath,
        requested_mode: Option<DeploymentMode>,
        resolved_mode: Option<DeploymentMode>,
        before: PathFingerprint,
        after: PathFingerprint,
        recovery_required: bool,
    ) -> Self {
        Self {
            order: 0,
            action,
            path,
            requested_mode,
            resolved_mode,
            before,
            after,
            recovery_required,
        }
    }

    #[must_use]
    pub fn inverse(&self) -> Self {
        Self {
            order: self.order,
            action: self.action.inverse(),
            path: self.path.clone(),
            requested_mode: self.requested_mode,
            resolved_mode: self.resolved_mode,
            before: self.after.clone(),
            after: self.before.clone(),
            recovery_required: self.recovery_required,
        }
    }

    #[must_use]
    pub const fn is_destructive(&self) -> bool {
        matches!(self.action, PlanAction::Replace | PlanAction::Remove)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanAction {
    Create,
    Replace,
    Remove,
    LeaveUntouched,
}

impl PlanAction {
    #[must_use]
    pub const fn inverse(self) -> Self {
        match self {
            Self::Create => Self::Remove,
            Self::Remove => Self::Create,
            Self::Replace => Self::Replace,
            Self::LeaveUntouched => Self::LeaveUntouched,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPath {
    target_id: TargetId,
    relative: BundleRelativePath,
    display_path: String,
    parent_identity: PathIdentity,
}

impl PlanPath {
    /// Captures an exact display path from an already authorized path value.
    ///
    /// # Errors
    ///
    /// Returns [`PlanBuildError`] when the authorized path cannot be represented as UTF-8 or its
    /// immediate parent is missing, unreadable, a symbolic link, or not a directory.
    pub fn from_authorized(
        target_id: TargetId,
        path: &AuthorizedPath,
    ) -> Result<Self, PlanBuildError> {
        let display_path = path
            .path()
            .to_str()
            .ok_or(PlanBuildError::NonUtf8AuthorizedPath)?
            .to_owned();
        let parent_identity = path
            .parent_identity()
            .map_err(|error| PlanBuildError::FinalParentUnavailable(error.to_string()))?;
        Ok(Self {
            target_id,
            relative: path.relative().clone(),
            display_path,
            parent_identity,
        })
    }

    #[must_use]
    pub const fn target_id(&self) -> TargetId {
        self.target_id
    }

    #[must_use]
    pub fn relative(&self) -> &BundleRelativePath {
        &self.relative
    }

    #[must_use]
    pub fn display_path(&self) -> &str {
        &self.display_path
    }

    #[must_use]
    pub const fn parent_identity(&self) -> PathIdentity {
        self.parent_identity
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathFingerprint {
    pub expected_kind: EntryKind,
    pub raw_symlink_target: Option<String>,
    pub metadata: Option<MetadataFingerprint>,
    pub bundle_digest: Option<BundleDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_subpath: Option<BundleRelativePath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_bundle_digest: Option<BundleDigest>,
    pub managed_skill_id: Option<SkillId>,
    pub managed_deployment_id: Option<DeploymentId>,
    pub captured_at: UtcTimestamp,
    pub adapter_id: AdapterId,
}

impl PathFingerprint {
    /// Compares path meaning while ignoring only observation time.
    #[must_use]
    pub fn semantically_eq(&self, other: &Self) -> bool {
        self.expected_kind == other.expected_kind
            && self.raw_symlink_target == other.raw_symlink_target
            && self.metadata == other.metadata
            && self.bundle_digest == other.bundle_digest
            && self.bundle_subpath == other.bundle_subpath
            && self.resolved_bundle_digest == other.resolved_bundle_digest
            && self.managed_skill_id == other.managed_skill_id
            && self.managed_deployment_id == other.managed_deployment_id
            && self.adapter_id == other.adapter_id
    }

    fn observably_distinct(&self, other: &Self) -> bool {
        self.expected_kind != other.expected_kind
            || self.raw_symlink_target != other.raw_symlink_target
            || self.metadata.is_some() != other.metadata.is_some()
            || matches!((self.metadata, other.metadata), (Some(left), Some(right)) if left != right)
            || matches!((self.bundle_digest, other.bundle_digest), (Some(left), Some(right)) if left != right)
            || self.bundle_subpath != other.bundle_subpath
            || self.resolved_bundle_digest != other.resolved_bundle_digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanBlocker {
    pub code: PlanBlockerCode,
    pub path: Option<PlanPath>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanBlockerCode {
    NameCollision,
    PermissionDenied,
    UnsupportedContent,
    Drift,
    InsufficientDiskSpace,
    UnsupportedFilesystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySummary {
    pub snapshot_count: u32,
    pub estimated_staging_bytes: u64,
    pub estimated_snapshot_bytes: u64,
    pub estimated_rollback_bytes: u64,
    pub spans_filesystems: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanDigest {
    schema_version: u16,
    bytes: [u8; 32],
}

impl PlanDigest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            schema_version: PLAN_SCHEMA_VERSION_V1,
            bytes,
        }
    }
}

impl fmt::Display for PlanDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.schema_version {
            PLAN_SCHEMA_VERSION_V1 => PLAN_DIGEST_PREFIX_V1,
            PLAN_SCHEMA_VERSION_V2 => PLAN_DIGEST_PREFIX_V2,
            PLAN_SCHEMA_VERSION_V3 => PLAN_DIGEST_PREFIX_V3,
            PLAN_SCHEMA_VERSION_V4 => PLAN_DIGEST_PREFIX_V4,
            PLAN_SCHEMA_VERSION_V5 => PLAN_DIGEST_PREFIX_V5,
            _ => return Err(fmt::Error),
        };
        write!(formatter, "{prefix}{}", hex::encode(self.bytes))
    }
}

impl FromStr for PlanDigest {
    type Err = PlanBuildError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (schema_version, encoded) =
            if let Some(encoded) = value.strip_prefix(PLAN_DIGEST_PREFIX_V1) {
                (PLAN_SCHEMA_VERSION_V1, encoded)
            } else if let Some(encoded) = value.strip_prefix(PLAN_DIGEST_PREFIX_V2) {
                (PLAN_SCHEMA_VERSION_V2, encoded)
            } else if let Some(encoded) = value.strip_prefix(PLAN_DIGEST_PREFIX_V3) {
                (PLAN_SCHEMA_VERSION_V3, encoded)
            } else if let Some(encoded) = value.strip_prefix(PLAN_DIGEST_PREFIX_V4) {
                (PLAN_SCHEMA_VERSION_V4, encoded)
            } else if let Some(encoded) = value.strip_prefix(PLAN_DIGEST_PREFIX_V5) {
                (PLAN_SCHEMA_VERSION_V5, encoded)
            } else {
                return Err(PlanBuildError::InvalidDigest);
            };
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(PlanBuildError::InvalidDigest);
        }
        let bytes = hex::decode(encoded).map_err(|_| PlanBuildError::InvalidDigest)?;
        Ok(Self {
            schema_version,
            bytes: bytes
                .try_into()
                .map_err(|_| PlanBuildError::InvalidDigest)?,
        })
    }
}

impl Serialize for PlanDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PlanDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Error)]
pub enum PlanBuildError {
    #[error("unsupported Operation Plan schema version {0}")]
    UnsupportedSchema(u16),
    #[error("Operation Plan must expire after it is created")]
    InvalidExpiry,
    #[error("Operation Plan must contain at least one step")]
    NoSteps,
    #[error("Operation Plan contains too many steps")]
    TooManySteps,
    #[error("Operation Plan contains more than one ownership choice for a Skill")]
    DuplicateOwnershipChoice,
    #[error("create step {0} must change an absent path to a present path")]
    InvalidCreateStep(u32),
    #[error("remove step {0} must change a present path to an absent path")]
    InvalidRemoveStep(u32),
    #[error("replace step {0} must change one present fingerprint to a different fingerprint")]
    InvalidReplaceStep(u32),
    #[error("replace step {0} before/after fingerprints are not distinguishable at execution time")]
    IndistinguishableReplaceStep(u32),
    #[error("leave-untouched step {0} must preserve the same semantic fingerprint")]
    InvalidLeaveUntouchedStep(u32),
    #[error("destructive step {0} must require recovery protection")]
    DestructiveStepWithoutRecovery(u32),
    #[error("destructive step {0} has no exact before-version identity and content proof")]
    UnverifiableDestructiveStep(u32),
    #[error("step {step} has an internally inconsistent {side} fingerprint")]
    InconsistentFingerprint { step: u32, side: &'static str },
    #[error("schema-v1 step {step} cannot use the bundle subpath fingerprint extension")]
    SchemaV1BundleSubpath { step: u32 },
    #[error("schema-v1/v2 step {step} cannot use the resolved Bundle fingerprint extension")]
    LegacyResolvedBundleDigest { step: u32 },
    #[error("Operation Plan contains duplicate steps for one logical or physical final path")]
    DuplicateStepPath,
    #[error("Operation Plan with destructive steps must declare at least one recovery Snapshot")]
    MissingRecoveryPoint,
    #[error("authorized path is not valid UTF-8")]
    NonUtf8AuthorizedPath,
    #[error("authorized final parent is unavailable or unsafe: {0}")]
    FinalParentUnavailable(String),
    #[error("could not serialize Operation Plan: {0}")]
    Serialize(serde_json::Error),
    #[error("invalid Operation Plan digest")]
    InvalidDigest,
    #[error("Operation Plan digest does not match its content")]
    DigestMismatch,
    #[error("Operation Plan takeover evidence is inconsistent")]
    InvalidTakeoverContext,
    #[error("Operation Plan deployment evidence is inconsistent")]
    InvalidDeploymentContext,
    #[error("Operation Plan batch deployment evidence is inconsistent")]
    InvalidBatchContext,
    #[error("Operation Plan Trash evidence is inconsistent")]
    InvalidTrashContext,
}

#[allow(clippy::too_many_lines)]
fn validate_trash(
    plan: &OperationPlanContent,
    context: &TrashPlanContext,
) -> Result<(), PlanBuildError> {
    let invalid = || Err(PlanBuildError::InvalidTrashContext);
    let expected_kind = match context.action {
        TrashAction::MoveToTrash => OperationKind::MoveToTrash,
        TrashAction::Restore => OperationKind::Restore,
        TrashAction::PermanentlyDelete => OperationKind::PermanentlyDelete,
    };
    let skill_path = BundleRelativePath::parse(&format!("skills/{}", context.skill_id))
        .map_err(|_| PlanBuildError::InvalidTrashContext)?;
    let trash_path =
        BundleRelativePath::parse(&format!(".manager/trash/{}", context.trash_entry_id))
            .map_err(|_| PlanBuildError::InvalidTrashContext)?;
    let manifest_path = BundleRelativePath::parse(&format!(
        ".manager/manifests/skills/{}.json",
        context.skill_id
    ))
    .map_err(|_| PlanBuildError::InvalidTrashContext)?;
    let authority =
        AdapterId::from_str("skills-hub@1").map_err(|_| PlanBuildError::InvalidTrashContext)?;
    let lifecycle_ok = matches!(
        (
            context.action,
            context.lifecycle_before,
            context.lifecycle_after
        ),
        (
            TrashAction::MoveToTrash,
            SkillLifecycle::Active,
            SkillLifecycle::Trashed
        ) | (
            TrashAction::Restore,
            SkillLifecycle::Trashed,
            SkillLifecycle::Active
        ) | (
            TrashAction::PermanentlyDelete,
            SkillLifecycle::Trashed,
            SkillLifecycle::PermanentlyRemoved
        )
    );
    let source = plan.steps.get(context.source_step_order as usize);
    let destination = context
        .destination_step_order
        .and_then(|order| plan.steps.get(order as usize));
    let source_expected = match context.action {
        TrashAction::MoveToTrash => &skill_path,
        TrashAction::Restore | TrashAction::PermanentlyDelete => &trash_path,
    };
    let paths_ok = source.is_some_and(|step| {
        step.order == context.source_step_order
            && step.order == 0
            && step.path.relative() == source_expected
            && &context.source_relative_path == source_expected
            && step.action == PlanAction::Remove
            && step.before.expected_kind == EntryKind::Directory
            && step.before.managed_skill_id == Some(context.skill_id)
            && step.before.bundle_digest == Some(context.working_digest)
            && step.before.adapter_id == authority
            && step.after.expected_kind == EntryKind::Absent
    }) && match context.action {
        TrashAction::PermanentlyDelete => {
            context.destination_relative_path.is_none() && context.destination_step_order.is_none()
        }
        _ => destination.is_some_and(|step| {
            Some(step.path.relative()) == context.destination_relative_path.as_ref()
                && step.order == 1
                && step.action == PlanAction::Create
                && step.before.expected_kind == EntryKind::Absent
                && step.after.expected_kind == EntryKind::Directory
                && step.after.managed_skill_id == Some(context.skill_id)
                && step.after.bundle_digest == Some(context.working_digest)
                && step.after.adapter_id == authority
        }),
    };
    let action_paths_ok = match context.action {
        TrashAction::MoveToTrash => context.destination_relative_path.as_ref() == Some(&trash_path),
        TrashAction::Restore => context
            .destination_relative_path
            .as_ref()
            .is_some_and(|path| {
                let mut parts = path.as_str().split('/');
                parts.next() == Some("skills")
                    && parts
                        .next()
                        .and_then(|id| SkillId::from_str(id).ok())
                        .is_some()
                    && parts.next().is_none()
            }),
        TrashAction::PermanentlyDelete => true,
    };
    let expected_source_subpath = match context.action {
        TrashAction::MoveToTrash => context.deployment_name.as_str().to_owned(),
        TrashAction::Restore | TrashAction::PermanentlyDelete => {
            format!("working/{}", context.deployment_name)
        }
    };
    let expected_steps = if context.action == TrashAction::PermanentlyDelete {
        1
    } else {
        2
    };
    let retention_ok = matches!(
        (context.retention_policy, context.retention_deadline),
        (TrashRetentionPolicy::Days30, Some(_)) | (TrashRetentionPolicy::Never, None)
    );
    if plan.kind != expected_kind
        || plan.selected_skill_ids != [context.skill_id]
        || !plan.selected_target_ids.is_empty()
        || !plan.selected_deployment_ids.is_empty()
        || !plan.ownership_choices.is_empty()
        || plan.steps.len() != expected_steps
        || !lifecycle_ok
        || !paths_ok
        || !action_paths_ok
        || !retention_ok
        || context.display_name.trim().is_empty()
        || context.confirmation_subject != context.display_name
        || !context.active_deployment_ids.is_empty()
        || !context.deployments_resolved
        || context.skill_manifest_path != manifest_path
        || context.provenance_paths != [manifest_path]
        || source
            .and_then(|step| step.before.bundle_subpath.as_ref())
            .map(BundleRelativePath::as_str)
            != Some(expected_source_subpath.as_str())
        || !matches!(
            (context.action, context.snapshot_id),
            (
                TrashAction::MoveToTrash | TrashAction::Restore | TrashAction::PermanentlyDelete,
                Some(_)
            )
        )
        || context.protected_reference_ids.iter().any(String::is_empty)
    {
        return invalid();
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_takeover(
    plan: &OperationPlanContent,
    takeover: &TakeoverPlanContext,
) -> Result<(), PlanBuildError> {
    let invalid = || Err(PlanBuildError::InvalidTakeoverContext);
    let skill = &takeover.skill;
    let mut observation_ids = BTreeSet::new();
    for observation in &takeover.observations {
        if !observation_ids.insert(observation.observation_id)
            || !Path::new(&observation.display_path).is_absolute()
            || observation
                .canonical_path
                .as_ref()
                .is_some_and(|path| !Path::new(path).is_absolute())
            || !matches!(
                (observation.target_scope, observation.project_id),
                (TakeoverTargetScope::Global, None) | (TakeoverTargetScope::Project, Some(_))
            )
        {
            return invalid();
        }
    }
    let Some(source) = takeover
        .observations
        .iter()
        .find(|item| item.observation_id == takeover.source_observation_id)
    else {
        return invalid();
    };
    if source.skill_id.is_some()
        || source.status != TakeoverObservationStatus::Present
        || source.entry_kind != EntryKind::Directory
        || source.metadata.is_none()
        || source.bundle_digest.is_none()
        || source.canonical_path.is_none()
        || source.deployment_name != skill.deployment_name
    {
        return invalid();
    }
    if plan.selected_skill_ids.as_slice() != [skill.skill_id]
        || !plan.selected_target_ids.contains(&skill.working_target_id)
        || !Path::new(&skill.vault_root).is_absolute()
        || skill.working_container_path.as_str() != format!("skills/{}", skill.skill_id)
        || skill.working_bundle_path.as_str()
            != format!("skills/{}/{}", skill.skill_id, skill.deployment_name)
        || skill.manifest_path.as_str()
            != format!(".manager/manifests/skills/{}.json", skill.skill_id)
        || skill.baseline_digest.is_none()
        || skill.baseline_object_path.is_none()
        || skill.baseline_digest != source.bundle_digest
    {
        return invalid();
    }
    let digest_hex = hex::encode(skill.baseline_digest.expect("checked").bytes());
    if skill
        .baseline_object_path
        .as_ref()
        .expect("checked")
        .as_str()
        != format!(
            ".manager/objects/sha256-bundle-v1/{}/{}",
            &digest_hex[..2],
            &digest_hex[2..]
        )
    {
        return invalid();
    }
    let Some(working) = plan
        .steps
        .iter()
        .find(|step| step.order == skill.working_step_order)
    else {
        return invalid();
    };
    if working.action != PlanAction::Create
        || working.path.target_id() != skill.working_target_id
        || working.path.relative() != &skill.working_container_path
        || Path::new(working.path.display_path())
            != Path::new(&skill.vault_root).join(skill.working_container_path.as_str())
        || working.before.expected_kind != EntryKind::Absent
        || working.before.bundle_subpath.is_some()
        || working.after.expected_kind != EntryKind::Directory
        || working.after.bundle_digest != skill.baseline_digest
        || working
            .after
            .bundle_subpath
            .as_ref()
            .map(BundleRelativePath::as_str)
            != Some(skill.deployment_name.as_str())
        || working.after.managed_skill_id != Some(skill.skill_id)
        || working.after.managed_deployment_id.is_some()
        || working.before.adapter_id != source.adapter_id
        || working.after.adapter_id != source.adapter_id
    {
        return invalid();
    }
    match takeover.decision {
        TakeoverDecision::AddToVault
            if !takeover.replacements.is_empty()
                || plan.selected_target_ids.as_slice() != [skill.working_target_id]
                || !plan.selected_deployment_ids.is_empty() =>
        {
            return invalid();
        }
        TakeoverDecision::AddAndManage if takeover.replacements.is_empty() => return invalid(),
        _ => {}
    }
    if skill.snapshot_id.is_some() == takeover.replacements.is_empty()
        || plan.recovery.snapshot_count != u32::from(!takeover.replacements.is_empty())
    {
        return invalid();
    }
    if takeover.replacements.len() + 1 != plan.steps.len() {
        return invalid();
    }
    let mut targets = BTreeSet::from([skill.working_target_id]);
    let mut target_contexts = BTreeMap::new();
    let mut deployments = BTreeSet::new();
    let mut replacement_observations = BTreeSet::new();
    let vault_root = Path::new(&skill.vault_root);
    let working_bundle_path =
        Path::new(working.path.display_path()).join(skill.deployment_name.as_str());
    for replacement in &takeover.replacements {
        targets.insert(replacement.target_id);
        let target_context = (
            replacement.adapter_id.clone(),
            replacement.target_scope,
            replacement.target_root.clone(),
            replacement.target_canonical_root.clone(),
            replacement.project_id,
            replacement.is_override,
            replacement.is_custom,
            replacement.existing_target,
        );
        if target_contexts
            .insert(replacement.target_id, target_context.clone())
            .is_some_and(|existing| existing != target_context)
        {
            return invalid();
        }
        if !deployments.insert(replacement.deployment_id)
            || !replacement_observations.insert(replacement.observation_id)
            || !Path::new(&replacement.target_root).is_absolute()
            || !Path::new(&replacement.target_canonical_root).is_absolute()
            || replacement.observation_id == takeover.source_observation_id
        {
            return invalid();
        }
        let Some(observation) = takeover
            .observations
            .iter()
            .find(|item| item.observation_id == replacement.observation_id)
        else {
            return invalid();
        };
        let target_root = Path::new(&replacement.target_root);
        let target_canonical_root = Path::new(&replacement.target_canonical_root);
        if target_canonical_root.starts_with(vault_root)
            || vault_root.starts_with(target_canonical_root)
        {
            return invalid();
        }
        let expected_final = target_canonical_root.join(replacement.target_relative_path.as_str());
        let same_physical_source = observation.canonical_path == source.canonical_path
            || matches!((observation.metadata, source.metadata), (Some(left), Some(right))
                if left.device_id == right.device_id && left.file_id == right.file_id);
        let authority_is_consistent = match replacement.target_scope {
            TakeoverTargetScope::Global => replacement.project_id.is_none(),
            TakeoverTargetScope::Project => replacement.project_id.is_some(),
        };
        if observation.skill_id.is_some()
            || observation.status != TakeoverObservationStatus::Present
            || observation.entry_kind != EntryKind::Directory
            || observation.metadata.is_none()
            || observation.bundle_digest != source.bundle_digest
            || same_physical_source
            || observation.canonical_path.as_deref().map(Path::new)
                != Some(expected_final.as_path())
            || observation.adapter_id != replacement.adapter_id
            || observation.target_scope != replacement.target_scope
            || observation.project_id != replacement.project_id
            || !authority_is_consistent
            || (!replacement.existing_target
                && (target_root != target_canonical_root
                    || replacement.is_override
                    || replacement.is_custom != (observation.source_root_kind == "custom")))
            || replacement.target_relative_path.as_str() != observation.deployment_name.as_str()
        {
            return invalid();
        }
        let Some(step) = plan
            .steps
            .iter()
            .find(|step| step.order == replacement.step_order)
        else {
            return invalid();
        };
        if step.action != PlanAction::Replace
            || step.path.target_id() != replacement.target_id
            || step.path.relative() != &replacement.target_relative_path
            || Path::new(step.path.display_path()) != expected_final
            || step.requested_mode != Some(replacement.deployment_mode)
            || step.resolved_mode != Some(replacement.deployment_mode)
            || step.before.expected_kind != EntryKind::Directory
            || step.before.metadata != observation.metadata
            || step.before.bundle_digest != observation.bundle_digest
            || step.before.bundle_subpath.is_some()
            || step.before.managed_skill_id.is_some()
            || step.before.managed_deployment_id.is_some()
            || step.before.adapter_id != observation.adapter_id
            || step.after.managed_skill_id != Some(skill.skill_id)
            || step.after.managed_deployment_id != Some(replacement.deployment_id)
            || step.after.bundle_subpath.is_some()
            || step.after.adapter_id != observation.adapter_id
        {
            return invalid();
        }
        match replacement.deployment_mode {
            DeploymentMode::Symlink
                if step.after.expected_kind != EntryKind::Symlink
                    || step.after.raw_symlink_target.as_deref() != working_bundle_path.to_str()
                    || step.after.bundle_digest.is_some() =>
            {
                return invalid();
            }
            DeploymentMode::ManagedCopy
                if step.after.expected_kind != EntryKind::Directory
                    || step.after.raw_symlink_target.is_some()
                    || step.after.bundle_digest != source.bundle_digest =>
            {
                return invalid();
            }
            _ => {}
        }
    }
    if plan
        .selected_target_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        != targets
        || plan
            .selected_deployment_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            != deployments
    {
        return invalid();
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_deployment(
    plan: &OperationPlanContent,
    context: &DeploymentPlanContext,
) -> Result<(), PlanBuildError> {
    let invalid = || Err(PlanBuildError::InvalidDeploymentContext);
    let skill = &context.skill;
    let target = &context.target;
    let deployment = &context.deployment;
    if plan.steps.len() != 1
        || plan.selected_skill_ids.as_slice() != [skill.skill_id]
        || plan.selected_target_ids.as_slice() != [target.target_id]
        || plan.selected_deployment_ids.as_slice() != [deployment.deployment_id]
        || !plan.ownership_choices.is_empty()
        || deployment.step_order != 0
        || deployment.deployment_updated_at < deployment.deployment_created_at
    {
        return invalid();
    }
    let step = &plan.steps[0];
    let preserves_changed_target = context.action == DeploymentProductAction::Undeploy
        && deployment.resolution == Some(UndeployResolution::PreserveTarget)
        && step.action == PlanAction::LeaveUntouched;
    let vault_root = Path::new(&skill.vault_root);
    let target_root = Path::new(&target.target_root);
    let target_canonical_root = Path::new(&target.target_canonical_root);
    if !vault_root.is_absolute()
        || !target_root.is_absolute()
        || !target_canonical_root.is_absolute()
        || target_canonical_root.starts_with(vault_root)
        || vault_root.starts_with(target_canonical_root)
        || skill.working_bundle_path.as_str()
            != format!("skills/{}/{}", skill.skill_id, skill.deployment_name)
        || deployment.target_relative_path.as_str() != skill.deployment_name.as_str()
        || deployment.manifest_path.as_str()
            != format!(
                ".manager/manifests/deployments/{}.json",
                deployment.deployment_id
            )
        || Path::new(step.path.display_path())
            != target_canonical_root.join(deployment.target_relative_path.as_str())
        || step.path.target_id() != target.target_id
        || step.path.relative() != &deployment.target_relative_path
        || step.requested_mode != Some(deployment.requested_mode)
        || step.resolved_mode != Some(deployment.resolved_mode)
        || step.before.adapter_id != target.adapter_id
        || step.after.adapter_id != target.adapter_id
        || (!preserves_changed_target
            && (target.capability.directory_write != CapabilityStatus::Supported
                || target.capability.atomic_rename != CapabilityStatus::Supported))
    {
        return invalid();
    }
    let authority_consistent = match target.target_scope {
        TakeoverTargetScope::Global => {
            target.project_id.is_none() && target.project_git_classification.is_none()
        }
        TakeoverTargetScope::Project => {
            (target.project_id.is_some()
                && matches!(
                    target.project_git_classification.as_deref(),
                    Some("git" | "none")
                ))
                || (target.is_custom
                    && target.project_id.is_none()
                    && target.project_git_classification.is_none())
        }
    };
    if !authority_consistent {
        return invalid();
    }
    if context.action == DeploymentProductAction::Deploy {
        match (deployment.requested_mode, deployment.resolved_mode) {
            (DeploymentMode::Symlink, DeploymentMode::Symlink)
                if target.capability.symlink != CapabilityStatus::Supported
                    || deployment.fallback_reason.is_some() =>
            {
                return invalid();
            }
            (DeploymentMode::Symlink, DeploymentMode::ManagedCopy)
                if target.capability.symlink != CapabilityStatus::Unsupported
                    || deployment
                        .fallback_reason
                        .as_deref()
                        .is_none_or(str::is_empty) =>
            {
                return invalid();
            }
            (DeploymentMode::ManagedCopy, DeploymentMode::ManagedCopy)
                if deployment.fallback_reason.is_some() =>
            {
                return invalid();
            }
            (DeploymentMode::ManagedCopy, DeploymentMode::Symlink) => return invalid(),
            _ => {}
        }
    } else if deployment.requested_mode != deployment.resolved_mode
        || deployment.fallback_reason.is_some()
    {
        return invalid();
    }
    let working = vault_root.join(skill.working_bundle_path.as_str());
    let expected_snapshot = match context.action {
        DeploymentProductAction::Deploy => {
            if plan.kind != OperationKind::Deploy
                || deployment.resolution.is_some()
                || deployment.active_before != deployment.existing_deployment
                || step.after.managed_skill_id != Some(skill.skill_id)
                || step.after.managed_deployment_id != Some(deployment.deployment_id)
            {
                return invalid();
            }
            match deployment.resolved_mode {
                DeploymentMode::Symlink
                    if step.after.expected_kind != EntryKind::Symlink
                        || step.after.raw_symlink_target.as_deref() != working.to_str()
                        || step.after.bundle_digest.is_some()
                        || step.after.bundle_subpath.is_some()
                        || step.after.resolved_bundle_digest != Some(skill.reviewed_digest) =>
                {
                    return invalid();
                }
                DeploymentMode::ManagedCopy
                    if step.after.expected_kind != EntryKind::Directory
                        || step.after.raw_symlink_target.is_some()
                        || step.after.bundle_digest != Some(skill.reviewed_digest)
                        || step.after.bundle_subpath.is_some()
                        || step.after.resolved_bundle_digest.is_some() =>
                {
                    return invalid();
                }
                _ => {}
            }
            if deployment.existing_deployment {
                if step.before.managed_skill_id != Some(skill.skill_id)
                    || step.before.managed_deployment_id != Some(deployment.deployment_id)
                    || deployment.previous_expected_digest.is_none()
                {
                    return invalid();
                }
                match (
                    deployment.reviewed_health,
                    step.action,
                    deployment.resolved_mode,
                ) {
                    (
                        DeploymentHealth::Clean,
                        PlanAction::LeaveUntouched | PlanAction::Replace,
                        _,
                    )
                    | (
                        DeploymentHealth::VaultAhead,
                        PlanAction::Replace,
                        DeploymentMode::ManagedCopy,
                    )
                    | (
                        DeploymentHealth::VaultAhead,
                        PlanAction::LeaveUntouched,
                        DeploymentMode::Symlink,
                    ) => {}
                    _ => return invalid(),
                }
            } else if step.action != PlanAction::Create
                || step.before.expected_kind != EntryKind::Absent
                || deployment.previous_expected_digest.is_some()
                || deployment.previous_expected_link_target.is_some()
            {
                return invalid();
            }
            step.action == PlanAction::Replace
        }
        DeploymentProductAction::Undeploy => {
            if plan.kind != OperationKind::Undeploy
                || !deployment.existing_deployment
                || !deployment.active_before
                || deployment.previous_expected_digest.is_none()
                || step.before.managed_skill_id != Some(skill.skill_id)
                || step.before.managed_deployment_id != Some(deployment.deployment_id)
                || step.after.managed_skill_id != Some(skill.skill_id)
                || step.after.managed_deployment_id != Some(deployment.deployment_id)
            {
                return invalid();
            }
            match deployment.resolution {
                Some(UndeployResolution::RemoveManaged)
                    if deployment.reviewed_health == DeploymentHealth::Clean
                        && step.action == PlanAction::Remove
                        && match deployment.resolved_mode {
                            DeploymentMode::Symlink => {
                                step.before.expected_kind == EntryKind::Symlink
                                    && step.before.raw_symlink_target.as_deref() == working.to_str()
                                    && step.before.bundle_digest.is_none()
                                    && step.before.bundle_subpath.is_none()
                            }
                            DeploymentMode::ManagedCopy => {
                                step.before.expected_kind == EntryKind::Directory
                                    && step.before.raw_symlink_target.is_none()
                                    && step.before.bundle_digest.is_some()
                                    && step.before.bundle_subpath.is_none()
                                    && step.before.resolved_bundle_digest.is_none()
                            }
                        } =>
                {
                    true
                }
                Some(UndeployResolution::PreserveTarget)
                    if deployment.reviewed_health != DeploymentHealth::Clean
                        && step.action == PlanAction::LeaveUntouched
                        && (step.before.resolved_bundle_digest.is_none()
                            || (deployment.reviewed_health == DeploymentHealth::VaultAhead
                                && step.before.raw_symlink_target.as_deref()
                                    == deployment.previous_expected_link_target.as_deref()
                                && step.before.resolved_bundle_digest
                                    == Some(skill.reviewed_digest))) =>
                {
                    false
                }
                _ => return invalid(),
            }
        }
    };
    if context.snapshot_id.is_some() != expected_snapshot
        || plan.recovery.snapshot_count != u32::from(expected_snapshot)
    {
        return invalid();
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_batch_deployment(
    plan: &OperationPlanContent,
    context: &BatchDeploymentPlanContext,
) -> Result<(), PlanBuildError> {
    let invalid = || Err(PlanBuildError::InvalidBatchContext);
    if !(2..=20).contains(&context.entries.len())
        || plan.steps.len() != context.entries.len()
        || plan.selected_skill_ids.as_slice() != [context.skill.skill_id]
        || !plan.ownership_choices.is_empty()
        || plan.takeover.is_some()
        || plan.deployment.is_some()
        || matches!(context.action, BatchDeploymentAction::Deploy)
            != (plan.kind == OperationKind::Deploy)
        || matches!(context.action, BatchDeploymentAction::Undo)
            != (plan.kind == OperationKind::Undo)
        || matches!(context.action, BatchDeploymentAction::Undo) != context.undo_of.is_some()
    {
        return invalid();
    }
    if context
        .entries
        .iter()
        .enumerate()
        .any(|(index, entry)| u32::try_from(index).ok() != Some(entry.deployment.step_order))
        || (context.action == BatchDeploymentAction::Deploy
            && context
                .entries
                .windows(2)
                .any(|pair| pair[0].target.target_id >= pair[1].target.target_id))
        || (context.action == BatchDeploymentAction::Undo
            && context.entries.windows(2).any(|pair| {
                pair[0]
                    .inverse
                    .as_ref()
                    .zip(pair[1].inverse.as_ref())
                    .is_none_or(|(left, right)| left.source_step_order <= right.source_step_order)
            }))
    {
        return invalid();
    }

    let mut targets = BTreeSet::new();
    let mut deployments = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut orders = BTreeSet::new();
    let mut source_orders = BTreeSet::new();
    for entry in &context.entries {
        if !targets.insert(entry.target.target_id)
            || !deployments.insert(entry.deployment.deployment_id)
            || !paths.insert((
                entry.target.target_canonical_root.clone(),
                entry.deployment.target_relative_path.clone(),
            ))
            || !orders.insert(entry.deployment.step_order)
        {
            return invalid();
        }
        let Some(step) = plan
            .steps
            .iter()
            .find(|step| step.order == entry.deployment.step_order)
        else {
            return invalid();
        };
        let vault_root = Path::new(&context.skill.vault_root);
        let target_root = Path::new(&entry.target.target_root);
        let target_canonical_root = Path::new(&entry.target.target_canonical_root);
        let authority_consistent = match entry.target.target_scope {
            TakeoverTargetScope::Global => {
                entry.target.project_id.is_none()
                    && entry.target.project_git_classification.is_none()
            }
            TakeoverTargetScope::Project => {
                (entry.target.project_id.is_some()
                    && matches!(
                        entry.target.project_git_classification.as_deref(),
                        Some("git" | "none")
                    ))
                    || (entry.target.is_custom
                        && entry.target.project_id.is_none()
                        && entry.target.project_git_classification.is_none())
            }
        };
        if !vault_root.is_absolute()
            || !target_root.is_absolute()
            || !target_canonical_root.is_absolute()
            || target_canonical_root.starts_with(vault_root)
            || vault_root.starts_with(target_canonical_root)
            || context.skill.working_bundle_path.as_str()
                != format!(
                    "skills/{}/{}",
                    context.skill.skill_id, context.skill.deployment_name
                )
            || entry.deployment.target_relative_path.as_str()
                != context.skill.deployment_name.as_str()
            || entry.deployment.manifest_path.as_str()
                != format!(
                    ".manager/manifests/deployments/{}.json",
                    entry.deployment.deployment_id
                )
            || Path::new(step.path.display_path())
                != target_canonical_root.join(entry.deployment.target_relative_path.as_str())
            || step.path.target_id() != entry.target.target_id
            || step.path.relative() != &entry.deployment.target_relative_path
            || step.requested_mode != Some(entry.deployment.requested_mode)
            || step.resolved_mode != Some(entry.deployment.resolved_mode)
            || step.before.adapter_id != entry.target.adapter_id
            || step.after.adapter_id != entry.target.adapter_id
            || entry.deployment.deployment_updated_at < entry.deployment.deployment_created_at
            || !authority_consistent
            || entry.target.capability.directory_write != CapabilityStatus::Supported
            || entry.target.capability.atomic_rename != CapabilityStatus::Supported
            || (entry.deployment.resolved_mode == DeploymentMode::Symlink
                && entry.target.capability.symlink != CapabilityStatus::Supported)
        {
            return invalid();
        }
        match (context.action, entry.inverse.as_ref(), step.action) {
            (BatchDeploymentAction::Deploy, None, _) => {}
            (BatchDeploymentAction::Undo, Some(inverse), PlanAction::Remove)
                if inverse.source_operation_id == context.undo_of.expect("validated undo ID")
                    && inverse.protected_reference.is_none()
                    && source_orders.insert(inverse.source_step_order)
                    && step.before.managed_skill_id == Some(context.skill.skill_id)
                    && step.before.managed_deployment_id
                        == Some(entry.deployment.deployment_id)
                    && step.after.expected_kind == EntryKind::Absent
                    && match entry.deployment.resolved_mode {
                        DeploymentMode::Symlink => step.before.expected_kind == EntryKind::Symlink,
                        DeploymentMode::ManagedCopy => {
                            step.before.expected_kind == EntryKind::Directory
                        }
                    } => {}
            (BatchDeploymentAction::Undo, Some(inverse), PlanAction::Replace)
                if inverse.source_operation_id == context.undo_of.expect("validated undo ID")
                    && inverse
                        .protected_reference
                        .as_deref()
                        .is_some_and(|reference| !reference.trim().is_empty())
                    && source_orders.insert(inverse.source_step_order)
                    && step.before.managed_skill_id == Some(context.skill.skill_id)
                    && step.before.managed_deployment_id
                        == Some(entry.deployment.deployment_id)
                    && step.after.managed_skill_id == Some(context.skill.skill_id)
                    && step.after.managed_deployment_id == Some(entry.deployment.deployment_id)
                    && match entry.deployment.resolved_mode {
                        DeploymentMode::Symlink => {
                            step.after.expected_kind == EntryKind::Symlink
                                && step.after.raw_symlink_target.is_some()
                        }
                        DeploymentMode::ManagedCopy => {
                            step.after.expected_kind == EntryKind::Directory
                                && step.after.bundle_digest.is_some()
                        }
                    } => {}
            _ => return invalid(),
        }

        if context.action == BatchDeploymentAction::Undo {
            continue;
        }

        // Reuse the complete single-target authority, capability, mode, path, manifest, health,
        // and fingerprint contract rather than maintaining a weaker batch variant.
        let destructive = step.is_destructive();
        let mut single_step = step.clone();
        single_step.order = 0;
        let single = OperationPlanContent {
            schema_version: PLAN_SCHEMA_VERSION_V3,
            operation_id: plan.operation_id,
            kind: if context.action == BatchDeploymentAction::Deploy {
                OperationKind::Deploy
            } else {
                OperationKind::Undeploy
            },
            created_at: plan.created_at,
            expires_at: plan.expires_at,
            selected_skill_ids: vec![context.skill.skill_id],
            selected_target_ids: vec![entry.target.target_id],
            selected_deployment_ids: vec![entry.deployment.deployment_id],
            ownership_choices: Vec::new(),
            bundle_caps: plan.bundle_caps,
            observed_bundle_stats: plan.observed_bundle_stats,
            steps: vec![single_step],
            blockers: Vec::new(),
            recovery: RecoverySummary {
                snapshot_count: u32::from(destructive),
                ..plan.recovery
            },
            non_atomic_consequences: Vec::new(),
            takeover: None,
            deployment: None,
            batch_deployment: None,
            trash: None,
        };
        let mut single_deployment = entry.deployment.clone();
        single_deployment.step_order = 0;
        let single_context = DeploymentPlanContext {
            action: if context.action == BatchDeploymentAction::Deploy {
                DeploymentProductAction::Deploy
            } else {
                DeploymentProductAction::Undeploy
            },
            skill: context.skill.clone(),
            target: entry.target.clone(),
            deployment: single_deployment,
            activity_id: context.activity_id,
            snapshot_id: if destructive {
                Some(
                    context
                        .snapshot_id
                        .ok_or(PlanBuildError::InvalidBatchContext)?,
                )
            } else {
                None
            },
        };
        validate_deployment(&single, &single_context)
            .map_err(|_| PlanBuildError::InvalidBatchContext)?;
    }

    if plan
        .selected_target_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        != targets
        || plan
            .selected_deployment_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            != deployments
        || plan.selected_target_ids.len() != targets.len()
        || plan.selected_deployment_ids.len() != deployments.len()
    {
        return invalid();
    }
    let destructive = plan.steps.iter().any(PlanStep::is_destructive);
    if (plan.recovery.snapshot_count > 0) != destructive
        || context.snapshot_id.is_some() != destructive
    {
        return invalid();
    }
    Ok(())
}

fn validate_steps(schema_version: u16, steps: &[PlanStep]) -> Result<(), PlanBuildError> {
    let mut logical_paths = BTreeSet::new();
    let mut physical_paths = BTreeSet::new();
    for step in steps {
        let final_name = step
            .path
            .relative()
            .as_str()
            .rsplit('/')
            .next()
            .expect("validated Bundle-relative paths contain a component");
        if !logical_paths.insert((step.path.target_id(), step.path.relative().clone()))
            || !physical_paths.insert((step.path.parent_identity(), final_name.to_owned()))
        {
            return Err(PlanBuildError::DuplicateStepPath);
        }
        validate_fingerprint(schema_version, &step.before, step.order, "before")?;
        validate_fingerprint(schema_version, &step.after, step.order, "after")?;
        let before_absent = step.before.expected_kind == EntryKind::Absent;
        let after_absent = step.after.expected_kind == EntryKind::Absent;
        match step.action {
            PlanAction::Create if !before_absent || after_absent => {
                return Err(PlanBuildError::InvalidCreateStep(step.order));
            }
            PlanAction::Remove if before_absent || !after_absent => {
                return Err(PlanBuildError::InvalidRemoveStep(step.order));
            }
            PlanAction::Replace
                if before_absent || after_absent || step.before.semantically_eq(&step.after) =>
            {
                return Err(PlanBuildError::InvalidReplaceStep(step.order));
            }
            PlanAction::Replace if !step.before.observably_distinct(&step.after) => {
                return Err(PlanBuildError::IndistinguishableReplaceStep(step.order));
            }
            PlanAction::LeaveUntouched if !step.before.semantically_eq(&step.after) => {
                return Err(PlanBuildError::InvalidLeaveUntouchedStep(step.order));
            }
            _ => {}
        }
        if step.is_destructive() && !step.recovery_required {
            return Err(PlanBuildError::DestructiveStepWithoutRecovery(step.order));
        }
        if step.is_destructive() && !has_exact_before_proof(&step.before) {
            return Err(PlanBuildError::UnverifiableDestructiveStep(step.order));
        }
    }
    Ok(())
}

fn validate_fingerprint(
    schema_version: u16,
    fingerprint: &PathFingerprint,
    step: u32,
    side: &'static str,
) -> Result<(), PlanBuildError> {
    if schema_version == PLAN_SCHEMA_VERSION_V1 && fingerprint.bundle_subpath.is_some() {
        return Err(PlanBuildError::SchemaV1BundleSubpath { step });
    }
    if schema_version < PLAN_SCHEMA_VERSION_V3 && fingerprint.resolved_bundle_digest.is_some() {
        return Err(PlanBuildError::LegacyResolvedBundleDigest { step });
    }
    let absent_has_evidence = fingerprint.expected_kind == EntryKind::Absent
        && (fingerprint.raw_symlink_target.is_some()
            || fingerprint.metadata.is_some()
            || fingerprint.bundle_digest.is_some()
            || fingerprint.bundle_subpath.is_some()
            || fingerprint.resolved_bundle_digest.is_some());
    let metadata_kind_mismatch = fingerprint
        .metadata
        .is_some_and(|metadata| metadata.kind != fingerprint.expected_kind);
    let link_target_mismatch = (fingerprint.expected_kind == EntryKind::Symlink)
        != fingerprint.raw_symlink_target.is_some();
    let digest_kind_mismatch =
        fingerprint.bundle_digest.is_some() && fingerprint.expected_kind != EntryKind::Directory;
    let subpath_without_digest =
        fingerprint.bundle_subpath.is_some() && fingerprint.bundle_digest.is_none();
    let resolved_digest_mismatch = fingerprint.resolved_bundle_digest.is_some()
        && (fingerprint.expected_kind != EntryKind::Symlink
            || fingerprint.raw_symlink_target.is_none());
    if absent_has_evidence
        || metadata_kind_mismatch
        || link_target_mismatch
        || digest_kind_mismatch
        || subpath_without_digest
        || resolved_digest_mismatch
    {
        return Err(PlanBuildError::InconsistentFingerprint { step, side });
    }
    Ok(())
}

fn has_exact_before_proof(fingerprint: &PathFingerprint) -> bool {
    match fingerprint.expected_kind {
        EntryKind::Directory => {
            fingerprint.metadata.is_some() && fingerprint.bundle_digest.is_some()
        }
        EntryKind::Symlink => {
            fingerprint.metadata.is_some() && fingerprint.raw_symlink_target.is_some()
        }
        EntryKind::File | EntryKind::Absent | EntryKind::Unsupported => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, str::FromStr};

    use tempfile::tempdir;

    use super::*;
    use crate::{
        domain::DurationMillis,
        filesystem::{AuthorizedRoot, BundleCaps, BundleStats},
    };

    fn fixed_content() -> OperationPlanContent {
        let operation_id = OperationId::from_str("018f0000-0000-7000-8000-000000000001").unwrap();
        let skill_id = SkillId::from_str("018f0000-0000-7000-8000-000000000002").unwrap();
        let target_id = TargetId::from_str("018f0000-0000-7000-8000-000000000003").unwrap();
        let deployment_id = DeploymentId::from_str("018f0000-0000-7000-8000-000000000004").unwrap();
        let captured_at = UtcTimestamp::from_unix_millis(1_721_234_567_890).unwrap();
        let expires_at = captured_at.checked_add(DurationMillis(300_000)).unwrap();
        let root_dir = tempdir().unwrap();
        fs::create_dir(root_dir.path().join("skills")).unwrap();
        let root = AuthorizedRoot::open(root_dir.path()).unwrap();
        let relative = BundleRelativePath::parse("skills/example").unwrap();
        let authorized = root.authorize(&relative).unwrap();
        let plan_path = PlanPath::from_authorized(target_id, &authorized).unwrap();
        let before = PathFingerprint {
            expected_kind: EntryKind::Absent,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: None,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: Some(skill_id),
            managed_deployment_id: Some(deployment_id),
            captured_at,
            adapter_id: AdapterId::new("claude-code", 1).unwrap(),
        };
        let mut after = before.clone();
        after.expected_kind = EntryKind::Directory;
        let step = PlanStep::new(
            PlanAction::Create,
            plan_path,
            Some(DeploymentMode::Symlink),
            Some(DeploymentMode::Symlink),
            before,
            after,
            false,
        );

        OperationPlanContent::new(
            operation_id,
            OperationKind::Deploy,
            captured_at,
            expires_at,
            vec![skill_id],
            vec![target_id],
            vec![deployment_id],
            Vec::new(),
            BundleCaps::default(),
            BundleStats::default(),
            vec![step],
            Vec::new(),
            RecoverySummary {
                snapshot_count: 0,
                estimated_staging_bytes: 100,
                estimated_snapshot_bytes: 0,
                estimated_rollback_bytes: 0,
                spans_filesystems: false,
            },
            Vec::new(),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn fixed_takeover_content() -> OperationPlanContent {
        let operation_id = OperationId::from_str("018f0000-0000-7000-8000-000000000101").unwrap();
        let skill_id = SkillId::from_str("018f0000-0000-7000-8000-000000000102").unwrap();
        let target_id = TargetId::from_str("018f0000-0000-7000-8000-000000000103").unwrap();
        let activity_id = ActivityId::from_str("018f0000-0000-7000-8000-000000000104").unwrap();
        let observation_id =
            ObservationId::from_str("018f0000-0000-7000-8000-000000000105").unwrap();
        let captured_at = UtcTimestamp::from_unix_millis(1_721_234_567_890).unwrap();
        let expires_at = captured_at.checked_add(DurationMillis(300_000)).unwrap();
        let adapter_id = AdapterId::new("universal-agent-skills", 1).unwrap();
        let deployment_name = DeploymentName::parse("example").unwrap();
        let digest = BundleDigest::from_bytes([7; 32]);
        let container = BundleRelativePath::parse(&format!("skills/{skill_id}")).unwrap();
        let working_bundle =
            BundleRelativePath::parse(&format!("skills/{skill_id}/example")).unwrap();
        let plan_path = PlanPath {
            target_id,
            relative: container.clone(),
            display_path: format!("/tmp/skills-hub-vault/skills/{skill_id}"),
            parent_identity: PathIdentity {
                device_id: 11,
                file_id: 12,
                kind: EntryKind::Directory,
            },
        };
        let before = PathFingerprint {
            expected_kind: EntryKind::Absent,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: None,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: None,
            managed_deployment_id: None,
            captured_at,
            adapter_id: adapter_id.clone(),
        };
        let after = PathFingerprint {
            expected_kind: EntryKind::Directory,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: Some(digest),
            bundle_subpath: Some(BundleRelativePath::parse("example").unwrap()),
            resolved_bundle_digest: None,
            managed_skill_id: Some(skill_id),
            managed_deployment_id: None,
            captured_at,
            adapter_id: adapter_id.clone(),
        };
        let step = PlanStep::new(
            PlanAction::Create,
            plan_path,
            None,
            None,
            before,
            after,
            false,
        );
        let object_hex = hex::encode(digest.bytes());
        let context = TakeoverPlanContext {
            decision: TakeoverDecision::AddToVault,
            source_observation_id: observation_id,
            observations: vec![TakeoverObservationEvidence {
                observation_id,
                skill_id: None,
                adapter_id,
                target_scope: TakeoverTargetScope::Global,
                project_id: None,
                source_root_kind: "global".into(),
                source_root_id: "universal-global".into(),
                display_path: "/tmp/skills-hub-external/example".into(),
                canonical_path: Some("/tmp/skills-hub-external/example".into()),
                deployment_name: deployment_name.clone(),
                bundle_digest: Some(digest),
                status: TakeoverObservationStatus::Present,
                error_code: None,
                error_summary: None,
                observed_at: captured_at,
                entry_kind: EntryKind::Directory,
                metadata: Some(MetadataFingerprint {
                    device_id: 21,
                    file_id: 22,
                    length: 96,
                    modified_seconds: 1_721_234_567,
                    modified_nanoseconds: 890_000_000,
                    kind: EntryKind::Directory,
                    executable: true,
                }),
                raw_symlink_target: None,
            }],
            skill: TakeoverSkillEvidence {
                skill_id,
                display_name: "example".into(),
                deployment_name,
                vault_root: "/tmp/skills-hub-vault".into(),
                working_target_id: target_id,
                working_container_path: container,
                working_bundle_path: working_bundle,
                manifest_path: BundleRelativePath::parse(&format!(
                    ".manager/manifests/skills/{skill_id}.json"
                ))
                .unwrap(),
                baseline_digest: Some(digest),
                baseline_object_path: Some(
                    BundleRelativePath::parse(&format!(
                        ".manager/objects/sha256-bundle-v1/{}/{}",
                        &object_hex[..2],
                        &object_hex[2..]
                    ))
                    .unwrap(),
                ),
                working_step_order: 0,
                activity_id,
                snapshot_id: None,
            },
            replacements: Vec::new(),
        };
        OperationPlanContent::new(
            operation_id,
            OperationKind::TakeOver,
            captured_at,
            expires_at,
            vec![skill_id],
            vec![target_id],
            Vec::new(),
            vec![OwnershipChoice {
                skill_id,
                decision: OwnershipDecision::TakeOver,
            }],
            BundleCaps::default(),
            BundleStats::default(),
            vec![step],
            Vec::new(),
            RecoverySummary {
                snapshot_count: 0,
                estimated_staging_bytes: 96,
                estimated_snapshot_bytes: 0,
                estimated_rollback_bytes: 0,
                spans_filesystems: false,
            },
            Vec::new(),
        )
        .with_takeover_context(context)
    }

    #[allow(clippy::too_many_lines)]
    fn fixed_takeover_with_replacement() -> OperationPlanContent {
        let mut content = fixed_takeover_content();
        let target_id = TargetId::from_str("018f0000-0000-7000-8000-000000000106").unwrap();
        let deployment_id = DeploymentId::from_str("018f0000-0000-7000-8000-000000000107").unwrap();
        let observation_id =
            ObservationId::from_str("018f0000-0000-7000-8000-000000000108").unwrap();
        let snapshot_id = SnapshotId::from_str("018f0000-0000-7000-8000-000000000109").unwrap();
        let evidence = content.takeover.as_mut().unwrap();
        let source = evidence.observations[0].clone();
        let skill_id = evidence.skill.skill_id;
        let digest = source.bundle_digest.unwrap();
        let metadata = MetadataFingerprint {
            device_id: 31,
            file_id: 32,
            length: 96,
            modified_seconds: 1_721_234_567,
            modified_nanoseconds: 890_000_000,
            kind: EntryKind::Directory,
            executable: true,
        };
        evidence.decision = TakeoverDecision::AddAndManage;
        evidence.skill.snapshot_id = Some(snapshot_id);
        evidence.observations.push(TakeoverObservationEvidence {
            observation_id,
            skill_id: None,
            adapter_id: source.adapter_id.clone(),
            target_scope: TakeoverTargetScope::Global,
            project_id: None,
            source_root_kind: "global".into(),
            source_root_id: "selected-global".into(),
            display_path: "/tmp/skills-hub-selected/example".into(),
            canonical_path: Some("/tmp/skills-hub-selected/example".into()),
            deployment_name: source.deployment_name.clone(),
            bundle_digest: Some(digest),
            status: TakeoverObservationStatus::Present,
            error_code: None,
            error_summary: None,
            observed_at: source.observed_at,
            entry_kind: EntryKind::Directory,
            metadata: Some(metadata),
            raw_symlink_target: None,
        });
        evidence.replacements.push(TakeoverReplacementEvidence {
            observation_id,
            target_id,
            deployment_id,
            adapter_id: source.adapter_id.clone(),
            target_scope: TakeoverTargetScope::Global,
            target_root: "/tmp/skills-hub-selected".into(),
            target_canonical_root: "/tmp/skills-hub-selected".into(),
            project_id: None,
            is_override: false,
            is_custom: false,
            existing_target: false,
            target_relative_path: BundleRelativePath::parse("example").unwrap(),
            deployment_mode: DeploymentMode::Symlink,
            step_order: 1,
        });
        let captured_at = content.created_at;
        content.steps.push(PlanStep::new(
            PlanAction::Replace,
            PlanPath {
                target_id,
                relative: BundleRelativePath::parse("example").unwrap(),
                display_path: "/tmp/skills-hub-selected/example".into(),
                parent_identity: PathIdentity {
                    device_id: 31,
                    file_id: 33,
                    kind: EntryKind::Directory,
                },
            },
            Some(DeploymentMode::Symlink),
            Some(DeploymentMode::Symlink),
            PathFingerprint {
                expected_kind: EntryKind::Directory,
                raw_symlink_target: None,
                metadata: Some(metadata),
                bundle_digest: Some(digest),
                bundle_subpath: None,
                resolved_bundle_digest: None,
                managed_skill_id: None,
                managed_deployment_id: None,
                captured_at,
                adapter_id: source.adapter_id.clone(),
            },
            PathFingerprint {
                expected_kind: EntryKind::Symlink,
                raw_symlink_target: Some(format!(
                    "/tmp/skills-hub-vault/skills/{skill_id}/example"
                )),
                metadata: None,
                bundle_digest: None,
                bundle_subpath: None,
                resolved_bundle_digest: None,
                managed_skill_id: Some(skill_id),
                managed_deployment_id: Some(deployment_id),
                captured_at,
                adapter_id: source.adapter_id,
            },
            true,
        ));
        content.selected_target_ids.push(target_id);
        content.selected_deployment_ids.push(deployment_id);
        content.recovery.snapshot_count = 1;
        content
    }

    #[allow(clippy::too_many_lines)]
    fn fixed_deployment_content() -> OperationPlanContent {
        let operation_id = OperationId::from_str("018f0000-0000-7000-8000-000000000201").unwrap();
        let skill_id = SkillId::from_str("018f0000-0000-7000-8000-000000000202").unwrap();
        let target_id = TargetId::from_str("018f0000-0000-7000-8000-000000000203").unwrap();
        let deployment_id = DeploymentId::from_str("018f0000-0000-7000-8000-000000000204").unwrap();
        let activity_id = ActivityId::from_str("018f0000-0000-7000-8000-000000000205").unwrap();
        let captured_at = UtcTimestamp::from_unix_millis(1_721_234_567_890).unwrap();
        let expires_at = captured_at.checked_add(DurationMillis(300_000)).unwrap();
        let adapter_id = AdapterId::new("universal-agent-skills", 1).unwrap();
        let deployment_name = DeploymentName::parse("example").unwrap();
        let digest = BundleDigest::from_bytes([17; 32]);
        let relative = BundleRelativePath::parse("example").unwrap();
        let before = PathFingerprint {
            expected_kind: EntryKind::Absent,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: None,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: None,
            managed_deployment_id: None,
            captured_at,
            adapter_id: adapter_id.clone(),
        };
        let after = PathFingerprint {
            expected_kind: EntryKind::Directory,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: Some(digest),
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: Some(skill_id),
            managed_deployment_id: Some(deployment_id),
            captured_at,
            adapter_id: adapter_id.clone(),
        };
        let step = PlanStep::new(
            PlanAction::Create,
            PlanPath {
                target_id,
                relative: relative.clone(),
                display_path: "/tmp/skills-hub-deploy-target/example".into(),
                parent_identity: PathIdentity {
                    device_id: 71,
                    file_id: 72,
                    kind: EntryKind::Directory,
                },
            },
            Some(DeploymentMode::ManagedCopy),
            Some(DeploymentMode::ManagedCopy),
            before,
            after,
            false,
        );
        let context = DeploymentPlanContext {
            action: DeploymentProductAction::Deploy,
            skill: DeploymentSkillEvidence {
                skill_id,
                deployment_name: deployment_name.clone(),
                vault_root: "/tmp/skills-hub-deploy-vault".into(),
                working_bundle_path: BundleRelativePath::parse(&format!(
                    "skills/{skill_id}/example"
                ))
                .unwrap(),
                reviewed_digest: digest,
            },
            target: DeploymentTargetEvidence {
                target_id,
                adapter_id,
                target_scope: TakeoverTargetScope::Global,
                target_root: "/tmp/skills-hub-deploy-target".into(),
                target_canonical_root: "/tmp/skills-hub-deploy-target".into(),
                project_id: None,
                project_git_classification: None,
                is_override: false,
                is_custom: false,
                capability: TargetCapabilityEvidence {
                    directory_write: CapabilityStatus::Supported,
                    atomic_rename: CapabilityStatus::Supported,
                    symlink: CapabilityStatus::Supported,
                },
            },
            deployment: ManagedDeploymentEvidence {
                deployment_id,
                deployment_created_at: captured_at,
                deployment_updated_at: captured_at,
                existing_deployment: false,
                active_before: false,
                target_relative_path: relative,
                requested_mode: DeploymentMode::ManagedCopy,
                resolved_mode: DeploymentMode::ManagedCopy,
                fallback_reason: None,
                previous_expected_digest: None,
                previous_expected_link_target: None,
                reviewed_health: DeploymentHealth::MissingTarget,
                resolution: None,
                step_order: 0,
                manifest_path: BundleRelativePath::parse(&format!(
                    ".manager/manifests/deployments/{deployment_id}.json"
                ))
                .unwrap(),
            },
            activity_id,
            snapshot_id: None,
        };
        OperationPlanContent::new(
            operation_id,
            OperationKind::Deploy,
            captured_at,
            expires_at,
            vec![skill_id],
            vec![target_id],
            vec![deployment_id],
            Vec::new(),
            BundleCaps::default(),
            BundleStats::default(),
            vec![step],
            Vec::new(),
            RecoverySummary {
                snapshot_count: 0,
                estimated_staging_bytes: 42,
                estimated_snapshot_bytes: 0,
                estimated_rollback_bytes: 0,
                spans_filesystems: false,
            },
            Vec::new(),
        )
        .with_deployment_context(context)
    }

    fn fixed_batch_deployment_content(count: usize) -> OperationPlanContent {
        let mut content = fixed_deployment_content();
        let single = content.deployment.take().unwrap();
        content.schema_version = PLAN_SCHEMA_VERSION_V1;
        content.steps.clear();
        content.selected_target_ids.clear();
        content.selected_deployment_ids.clear();
        let mut entries = Vec::new();
        for index in 0..count {
            let target_id =
                TargetId::from_str(&format!("018f0000-0000-7000-8000-{:012x}", 0x300 + index))
                    .unwrap();
            let deployment_id =
                DeploymentId::from_str(&format!("018f0000-0000-7000-8000-{:012x}", 0x400 + index))
                    .unwrap();
            let target_root = format!("/tmp/skills-hub-batch-target-{index}");
            let mut target = single.target.clone();
            target.target_id = target_id;
            target.target_root.clone_from(&target_root);
            target.target_canonical_root.clone_from(&target_root);
            let mut deployment = single.deployment.clone();
            deployment.deployment_id = deployment_id;
            deployment.step_order = u32::try_from(index).unwrap();
            deployment.manifest_path = BundleRelativePath::parse(&format!(
                ".manager/manifests/deployments/{deployment_id}.json"
            ))
            .unwrap();
            let mut step = fixed_deployment_content().steps.remove(0);
            step.path.target_id = target_id;
            step.path.display_path = format!("{target_root}/example");
            step.path.parent_identity.file_id += u64::try_from(index).unwrap();
            step.after.managed_deployment_id = Some(deployment_id);
            content.steps.push(step);
            content.selected_target_ids.push(target_id);
            content.selected_deployment_ids.push(deployment_id);
            entries.push(BatchDeploymentEntryEvidence {
                target,
                deployment,
                inverse: None,
            });
        }
        content.with_batch_deployment_context(BatchDeploymentPlanContext {
            action: BatchDeploymentAction::Deploy,
            skill: single.skill,
            entries,
            activity_id: single.activity_id,
            snapshot_id: None,
            undo_of: None,
        })
    }

    #[test]
    fn identical_frozen_content_has_identical_bytes_and_digest() {
        let content = fixed_content();
        let first = OperationPlan::build(content.clone()).unwrap();
        let second = OperationPlan::build(content).unwrap();
        assert_eq!(first.plan_digest, second.plan_digest);
        assert_eq!(
            first.canonical_json().unwrap(),
            second.canonical_json().unwrap()
        );
        let canonical = String::from_utf8(first.canonical_json().unwrap()).unwrap();
        assert!(!canonical.contains("\"takeover\""));
        assert!(
            first
                .plan_digest
                .to_string()
                .starts_with(PLAN_DIGEST_PREFIX_V1)
        );
        first.verify_digest().unwrap();
    }

    #[test]
    fn schema_v1_digest_omits_and_rejects_bundle_subpath_extension() {
        let mut content = fixed_content();
        content.steps[0].path.display_path = "/tmp/skills-hub-v1/skills/example".into();
        content.steps[0].path.parent_identity = PathIdentity {
            device_id: 41,
            file_id: 42,
            kind: EntryKind::Directory,
        };
        let plan = OperationPlan::build(content.clone()).unwrap();
        assert_eq!(
            plan.plan_digest.to_string(),
            "sha256-operation-plan-v1:d9a88952d06f0db1f00777e5438bae46c93f964f366b3a374043e7bc4209062d"
        );
        assert!(
            !String::from_utf8(plan.canonical_json().unwrap())
                .unwrap()
                .contains("bundleSubpath")
        );

        let mut resolved = content.clone();
        resolved.steps[0].after.resolved_bundle_digest = Some(BundleDigest::from_bytes([8; 32]));
        resolved.steps[0].after.expected_kind = EntryKind::Symlink;
        resolved.steps[0].after.raw_symlink_target = Some("/tmp/working".into());
        assert!(matches!(
            OperationPlan::build(resolved),
            Err(PlanBuildError::LegacyResolvedBundleDigest { step: 0 })
        ));

        content.steps[0].after.bundle_subpath = Some(BundleRelativePath::parse("nested").unwrap());
        content.steps[0].after.bundle_digest = Some(BundleDigest::from_bytes([8; 32]));
        assert!(matches!(
            OperationPlan::build(content),
            Err(PlanBuildError::SchemaV1BundleSubpath { step: 0 })
        ));
    }

    #[test]
    fn digest_parses_all_schema_prefixes_and_from_bytes_remains_v1() {
        let bytes = [9; 32];
        let v1 = PlanDigest::from_bytes(bytes);
        assert!(v1.to_string().starts_with(PLAN_DIGEST_PREFIX_V1));
        let v2_text = format!("{PLAN_DIGEST_PREFIX_V2}{}", hex::encode(bytes));
        let v2 = PlanDigest::from_str(&v2_text).unwrap();
        assert_eq!(v2.to_string(), v2_text);
        assert_ne!(v1, v2);
        let v3_text = format!("{PLAN_DIGEST_PREFIX_V3}{}", hex::encode(bytes));
        let v3 = PlanDigest::from_str(&v3_text).unwrap();
        assert_eq!(v3.to_string(), v3_text);
        assert_ne!(v2, v3);
    }

    #[test]
    fn takeover_schema_v2_has_a_frozen_digest_and_seals_context() {
        let content = fixed_takeover_content();
        let plan = OperationPlan::build(content.clone()).unwrap();
        assert_eq!(
            plan.plan_digest.to_string(),
            "sha256-operation-plan-v2:451ee764719a0b8fe9b7da827ed818a7e28ce8bd5ebec61a0858863319ca5e0f"
        );
        assert!(
            String::from_utf8(plan.canonical_json().unwrap())
                .unwrap()
                .contains("\"takeover\"")
        );

        let mut changed = content.clone();
        changed.takeover.as_mut().unwrap().observations[0].source_root_id = "changed".into();
        assert_ne!(
            OperationPlan::build(changed).unwrap().plan_digest,
            plan.plan_digest
        );

        let mut inconsistent = plan.content;
        inconsistent
            .takeover
            .as_mut()
            .unwrap()
            .skill
            .working_bundle_path = BundleRelativePath::parse("skills/wrong/example").unwrap();
        assert!(matches!(
            OperationPlan::build(inconsistent),
            Err(PlanBuildError::InvalidTakeoverContext)
        ));

        let mut resolved = content;
        resolved.steps[0].after.resolved_bundle_digest = Some(BundleDigest::from_bytes([7; 32]));
        assert!(matches!(
            OperationPlan::build(resolved),
            Err(PlanBuildError::LegacyResolvedBundleDigest { step: 0 })
        ));
    }

    #[test]
    fn deployment_schema_v3_has_a_frozen_digest_and_rejects_inconsistent_context() {
        let content = fixed_deployment_content();
        let plan = OperationPlan::build(content.clone()).unwrap();
        assert_eq!(
            plan.plan_digest.to_string(),
            "sha256-operation-plan-v3:7c766d0e52595f79a8a8c223dda2a663c2fb8ab7c4ddf1d01bbf7708b2bca0e9"
        );
        let canonical = String::from_utf8(plan.canonical_json().unwrap()).unwrap();
        assert!(canonical.contains("\"deployment\""));
        assert!(!canonical.contains("resolvedBundleDigest"));

        let mut symlink = content.clone();
        let evidence = symlink.deployment.as_mut().unwrap();
        evidence.deployment.requested_mode = DeploymentMode::Symlink;
        evidence.deployment.resolved_mode = DeploymentMode::Symlink;
        let working = Path::new(&evidence.skill.vault_root)
            .join(evidence.skill.working_bundle_path.as_str())
            .to_string_lossy()
            .into_owned();
        symlink.steps[0].requested_mode = Some(DeploymentMode::Symlink);
        symlink.steps[0].resolved_mode = Some(DeploymentMode::Symlink);
        let after = &mut symlink.steps[0].after;
        after.expected_kind = EntryKind::Symlink;
        after.raw_symlink_target = Some(working);
        after.bundle_digest = None;
        after.resolved_bundle_digest = Some(evidence.skill.reviewed_digest);
        let symlink = OperationPlan::build(symlink).unwrap();
        assert!(
            String::from_utf8(symlink.canonical_json().unwrap())
                .unwrap()
                .contains("resolvedBundleDigest")
        );

        let mut custom = content.clone();
        custom.deployment.as_mut().unwrap().target.is_custom = true;
        OperationPlan::build(custom).expect("custom targets use the generic operation contract");

        let mut unknown_project = content.clone();
        let target = &mut unknown_project.deployment.as_mut().unwrap().target;
        target.target_scope = TakeoverTargetScope::Project;
        target.project_id =
            Some(ProjectId::from_str("018f0000-0000-7000-8000-000000000206").unwrap());
        target.project_git_classification = Some("unknown".into());
        assert!(matches!(
            OperationPlan::build(unknown_project),
            Err(PlanBuildError::InvalidDeploymentContext)
        ));

        let mut retargeted_preserve = fixed_deployment_content();
        retargeted_preserve.kind = OperationKind::Undeploy;
        let evidence = retargeted_preserve.deployment.as_mut().unwrap();
        evidence.action = DeploymentProductAction::Undeploy;
        evidence.deployment.existing_deployment = true;
        evidence.deployment.active_before = true;
        evidence.deployment.previous_expected_digest = Some(evidence.skill.reviewed_digest);
        evidence.deployment.reviewed_health = DeploymentHealth::Conflict;
        evidence.deployment.resolution = Some(UndeployResolution::PreserveTarget);
        let mut retargeted = retargeted_preserve.steps[0].after.clone();
        retargeted.expected_kind = EntryKind::Symlink;
        retargeted.raw_symlink_target = Some("/tmp/user-retarget".into());
        retargeted.metadata = Some(MetadataFingerprint {
            device_id: 81,
            file_id: 82,
            length: 18,
            modified_seconds: 1_721_234_567,
            modified_nanoseconds: 890_000_000,
            kind: EntryKind::Symlink,
            executable: false,
        });
        retargeted.bundle_digest = None;
        retargeted.resolved_bundle_digest = Some(evidence.skill.reviewed_digest);
        retargeted_preserve.steps[0].action = PlanAction::LeaveUntouched;
        retargeted_preserve.steps[0].before = retargeted.clone();
        retargeted_preserve.steps[0].after = retargeted;
        assert!(matches!(
            OperationPlan::build(retargeted_preserve),
            Err(PlanBuildError::InvalidDeploymentContext)
        ));

        let mut changed = content;
        changed
            .deployment
            .as_mut()
            .unwrap()
            .target
            .capability
            .atomic_rename = CapabilityStatus::Unknown;
        assert!(matches!(
            OperationPlan::build(changed),
            Err(PlanBuildError::InvalidDeploymentContext)
        ));
    }

    #[test]
    fn batch_deployment_schema_v4_has_a_frozen_digest_and_preserves_legacy_vectors() {
        let plan = OperationPlan::build(fixed_batch_deployment_content(2)).unwrap();
        assert_eq!(
            plan.plan_digest.to_string(),
            "sha256-operation-plan-v4:6d493f994ee66bb56b1c349a3cd296d9fe8b9660bd08107a3e76297bfbcc27ec"
        );
        assert_eq!(
            OperationPlan::build(fixed_content())
                .unwrap()
                .content
                .schema_version,
            PLAN_SCHEMA_VERSION_V1
        );
        assert_eq!(
            OperationPlan::build(fixed_takeover_content())
                .unwrap()
                .plan_digest
                .to_string(),
            "sha256-operation-plan-v2:451ee764719a0b8fe9b7da827ed818a7e28ce8bd5ebec61a0858863319ca5e0f"
        );
        assert_eq!(
            OperationPlan::build(fixed_deployment_content())
                .unwrap()
                .plan_digest
                .to_string(),
            "sha256-operation-plan-v3:7c766d0e52595f79a8a8c223dda2a663c2fb8ab7c4ddf1d01bbf7708b2bca0e9"
        );
    }

    #[test]
    fn batch_deployment_rejects_bounds_duplicates_and_step_context_mismatch() {
        for count in [1, 21] {
            assert!(matches!(
                OperationPlan::build(fixed_batch_deployment_content(count)),
                Err(PlanBuildError::InvalidBatchContext)
            ));
        }

        let mut duplicate = fixed_batch_deployment_content(2);
        let context = duplicate.batch_deployment.as_mut().unwrap();
        context.entries[1].target.target_id = context.entries[0].target.target_id;
        assert!(matches!(
            OperationPlan::build(duplicate),
            Err(PlanBuildError::InvalidBatchContext)
        ));

        let mut duplicate = fixed_batch_deployment_content(2);
        let context = duplicate.batch_deployment.as_mut().unwrap();
        context.entries[1].deployment.deployment_id = context.entries[0].deployment.deployment_id;
        assert!(matches!(
            OperationPlan::build(duplicate),
            Err(PlanBuildError::InvalidBatchContext)
        ));

        let mut duplicate = fixed_batch_deployment_content(2);
        let context = duplicate.batch_deployment.as_mut().unwrap();
        context.entries[1].target.target_canonical_root =
            context.entries[0].target.target_canonical_root.clone();
        assert!(matches!(
            OperationPlan::build(duplicate),
            Err(PlanBuildError::InvalidBatchContext)
        ));

        let mut mismatch = fixed_batch_deployment_content(2);
        mismatch.batch_deployment.as_mut().unwrap().entries[1]
            .deployment
            .step_order = 0;
        assert!(matches!(
            OperationPlan::build(mismatch),
            Err(PlanBuildError::InvalidBatchContext)
        ));
    }

    #[test]
    fn schema_v2_rejects_source_replacement_and_physical_alias_evidence() {
        OperationPlan::build(fixed_takeover_with_replacement()).unwrap();

        let mut source_id = fixed_takeover_with_replacement();
        let context = source_id.takeover.as_mut().unwrap();
        context.replacements[0].observation_id = context.source_observation_id;
        assert!(matches!(
            OperationPlan::build(source_id),
            Err(PlanBuildError::InvalidTakeoverContext)
        ));

        let mut canonical_alias = fixed_takeover_with_replacement();
        let context = canonical_alias.takeover.as_mut().unwrap();
        context.observations[1].canonical_path = context.observations[0].canonical_path.clone();
        assert!(matches!(
            OperationPlan::build(canonical_alias),
            Err(PlanBuildError::InvalidTakeoverContext)
        ));

        let mut physical_alias = fixed_takeover_with_replacement();
        let context = physical_alias.takeover.as_mut().unwrap();
        let source_metadata = context.observations[0].metadata;
        context.observations[1].metadata = source_metadata;
        physical_alias.steps[1].before.metadata = source_metadata;
        assert!(matches!(
            OperationPlan::build(physical_alias),
            Err(PlanBuildError::InvalidTakeoverContext)
        ));
    }

    #[test]
    fn changed_fingerprint_invalidates_the_digest() {
        let content = fixed_content();
        let first = OperationPlan::build(content.clone()).unwrap();
        let mut changed = content;
        changed.steps[0].after.expected_kind = EntryKind::File;
        let second = OperationPlan::build(changed).unwrap();

        assert_ne!(first.plan_digest, second.plan_digest);
    }

    #[test]
    fn verification_rejects_noncanonical_content_even_when_digest_rebuilds() {
        let mut plan = OperationPlan::build(fixed_content()).unwrap();
        plan.content.steps[0].order = 42;

        assert!(matches!(
            plan.verify_digest(),
            Err(PlanBuildError::DigestMismatch)
        ));
    }

    #[test]
    fn step_order_changes_the_digest() {
        let mut first_content = fixed_content();
        let mut other = first_content.steps[0].clone();
        other.path.relative = BundleRelativePath::parse("skills/other").unwrap();
        other.path.display_path = other.path.display_path.replace("example", "other");
        first_content.steps.push(other);
        let mut second_content = first_content.clone();
        second_content.steps.reverse();

        let first = OperationPlan::build(first_content).unwrap();
        let second = OperationPlan::build(second_content).unwrap();
        assert_ne!(first.plan_digest, second.plan_digest);
    }

    #[test]
    fn inverse_step_swaps_action_and_fingerprints() {
        let content = fixed_content();
        let original = &content.steps[0];
        let inverse = original.inverse();

        assert_eq!(inverse.action, PlanAction::Remove);
        assert_eq!(inverse.before, original.after);
        assert_eq!(inverse.after, original.before);
        assert_eq!(inverse.inverse(), *original);
    }

    #[test]
    fn invalid_expiry_and_empty_steps_are_rejected() {
        let mut content = fixed_content();
        content.expires_at = content.created_at;
        assert!(matches!(
            OperationPlan::build(content),
            Err(PlanBuildError::InvalidExpiry)
        ));

        let mut content = fixed_content();
        content.steps.clear();
        assert!(matches!(
            OperationPlan::build(content),
            Err(PlanBuildError::NoSteps)
        ));
    }

    #[test]
    fn action_fingerprint_invariants_are_sealed_before_digest() {
        let mut create = fixed_content();
        create.steps[0].after = create.steps[0].before.clone();
        assert!(matches!(
            OperationPlan::build(create),
            Err(PlanBuildError::InvalidCreateStep(0))
        ));

        let mut remove = fixed_content();
        remove.steps[0].action = PlanAction::Remove;
        remove.steps[0].recovery_required = true;
        assert!(matches!(
            OperationPlan::build(remove),
            Err(PlanBuildError::InvalidRemoveStep(0))
        ));

        let mut replace = fixed_content();
        replace.steps[0].action = PlanAction::Replace;
        replace.steps[0].before = replace.steps[0].after.clone();
        replace.steps[0].recovery_required = true;
        replace.recovery.snapshot_count = 1;
        assert!(matches!(
            OperationPlan::build(replace),
            Err(PlanBuildError::InvalidReplaceStep(0))
        ));

        let mut untouched = fixed_content();
        untouched.steps[0].action = PlanAction::LeaveUntouched;
        untouched.steps[0].before = untouched.steps[0].after.clone();
        untouched.steps[0].after.expected_kind = EntryKind::File;
        assert!(matches!(
            OperationPlan::build(untouched),
            Err(PlanBuildError::InvalidLeaveUntouchedStep(0))
        ));
    }

    #[test]
    fn destructive_steps_require_flag_and_one_operation_recovery_point() {
        let mut content = fixed_content();
        content.steps[0].action = PlanAction::Replace;
        content.steps[0].before = content.steps[0].after.clone();
        content.steps[0].after.expected_kind = EntryKind::File;
        assert!(matches!(
            OperationPlan::build(content.clone()),
            Err(PlanBuildError::DestructiveStepWithoutRecovery(0))
        ));

        content.steps[0].recovery_required = true;
        content.steps[0].before.metadata = Some(MetadataFingerprint {
            device_id: 1,
            file_id: 2,
            length: 3,
            modified_seconds: 4,
            modified_nanoseconds: 5,
            kind: EntryKind::Directory,
            executable: false,
        });
        content.steps[0].before.bundle_digest = Some(BundleDigest::from_bytes([7; 32]));
        assert!(matches!(
            OperationPlan::build(content.clone()),
            Err(PlanBuildError::MissingRecoveryPoint)
        ));

        let mut second = content.steps[0].clone();
        second.path.relative = BundleRelativePath::parse("skills/other").unwrap();
        second.path.display_path = second.path.display_path.replace("example", "other");
        second.action = PlanAction::Remove;
        second.after.expected_kind = EntryKind::Absent;
        second.after.managed_deployment_id = second.before.managed_deployment_id;
        content.steps.push(second);
        content.recovery.snapshot_count = 1;
        OperationPlan::build(content).unwrap();
    }

    #[test]
    fn destructive_steps_require_exact_identity_and_content_proof() {
        let mut content = fixed_content();
        content.steps[0].action = PlanAction::Replace;
        content.steps[0].before = content.steps[0].after.clone();
        content.steps[0].after.expected_kind = EntryKind::File;
        content.steps[0].recovery_required = true;
        content.recovery.snapshot_count = 1;

        assert!(matches!(
            OperationPlan::build(content),
            Err(PlanBuildError::UnverifiableDestructiveStep(0))
        ));

        let mut file = fixed_content();
        file.steps[0].action = PlanAction::Replace;
        file.steps[0].before.expected_kind = EntryKind::File;
        file.steps[0].before.metadata = Some(MetadataFingerprint {
            device_id: 1,
            file_id: 2,
            length: 3,
            modified_seconds: 4,
            modified_nanoseconds: 5,
            kind: EntryKind::File,
            executable: false,
        });
        file.steps[0].recovery_required = true;
        file.recovery.snapshot_count = 1;
        assert!(matches!(
            OperationPlan::build(file),
            Err(PlanBuildError::UnverifiableDestructiveStep(0))
        ));
    }

    #[test]
    fn internally_inconsistent_fingerprints_are_rejected_before_sealing() {
        let mut content = fixed_content();
        content.steps[0].before.metadata = Some(MetadataFingerprint {
            device_id: 1,
            file_id: 2,
            length: 0,
            modified_seconds: 3,
            modified_nanoseconds: 4,
            kind: EntryKind::Absent,
            executable: false,
        });

        assert!(matches!(
            OperationPlan::build(content),
            Err(PlanBuildError::InconsistentFingerprint {
                step: 0,
                side: "before"
            })
        ));
    }

    #[test]
    fn replace_must_differ_in_a_runtime_observable_field() {
        let mut content = fixed_content();
        content.steps[0].action = PlanAction::Replace;
        content.steps[0].before = content.steps[0].after.clone();
        content.steps[0].after.managed_deployment_id = None;
        content.steps[0].recovery_required = true;
        content.recovery.snapshot_count = 1;

        assert!(matches!(
            OperationPlan::build(content),
            Err(PlanBuildError::IndistinguishableReplaceStep(0))
        ));
    }

    #[test]
    fn leave_untouched_ignores_only_capture_time_and_duplicate_paths_are_rejected() {
        let mut content = fixed_content();
        content.steps[0].action = PlanAction::LeaveUntouched;
        content.steps[0].before = content.steps[0].after.clone();
        content.steps[0].after.captured_at = content.steps[0]
            .after
            .captured_at
            .checked_add(DurationMillis(1))
            .unwrap();
        OperationPlan::build(content.clone()).unwrap();

        content.steps.push(content.steps[0].clone());
        assert!(matches!(
            OperationPlan::build(content),
            Err(PlanBuildError::DuplicateStepPath)
        ));

        let mut aliased = fixed_content();
        let mut duplicate = aliased.steps[0].clone();
        duplicate.path.target_id = TargetId::generate();
        aliased.steps.push(duplicate);
        assert!(matches!(
            OperationPlan::build(aliased),
            Err(PlanBuildError::DuplicateStepPath)
        ));
    }

    #[test]
    fn final_parent_identity_is_required_and_bound_into_digest() {
        let directory = tempdir().unwrap();
        let root = AuthorizedRoot::open(directory.path()).unwrap();
        let missing_parent = root
            .authorize(&BundleRelativePath::parse("missing/skill").unwrap())
            .unwrap();
        assert!(matches!(
            PlanPath::from_authorized(TargetId::generate(), &missing_parent),
            Err(PlanBuildError::FinalParentUnavailable(_))
        ));

        let content = fixed_content();
        let first = OperationPlan::build(content.clone()).unwrap();
        assert!(
            String::from_utf8(first.canonical_json().unwrap())
                .unwrap()
                .contains("parentIdentity")
        );
        let mut changed = content;
        changed.steps[0].path.parent_identity.file_id ^= 1;
        let second = OperationPlan::build(changed).unwrap();
        assert_ne!(first.plan_digest, second.plan_digest);
    }
}
