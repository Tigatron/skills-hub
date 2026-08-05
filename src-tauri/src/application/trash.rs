//! Typed application boundary for the durable Trash workflow.
//!
//! Filesystem writes are deliberately performed by the generic operation executor; this module
//! owns request validation and stable read models only.

use std::{fs, path::Path, str::FromStr, sync::Arc};

use serde::{Deserialize, Serialize};
use specta::Type;
use thiserror::Error;

use crate::{
    domain::{
        ActivityId, AdapterId, BundleRelativePath, DurationMillis, OperationId, SkillId,
        SkillLifecycle, SnapshotId, TargetId, TrashEntryId, UtcTimestamp,
    },
    filesystem::{
        AuthorizedRoot, BundleCaps, EntryKind, MetadataFingerprint, copy_bundle_exact, hash_bundle,
    },
    operations::{
        CancellationToken, OperationCoordinator, OperationError, OperationExecutor,
        OperationFinalizer, OperationHookError, OperationIntent, OperationKind, OperationPlan,
        OperationPlanContent, OperationPlanner, OperationStore, PathFingerprint, PlanAction,
        PlanBuilder, PlanPath, PlanStep, RecoverySummary, SnapshotProtection, SnapshotRegistrar,
        SnapshotRegistration, StagingProvider, TargetRoots, TrashAction, TrashPlanContext,
        TrashRetentionPolicy,
    },
    persistence::{
        ObjectRecord, OpenVault, RepositoryError, SkillRecord, TrashEntryManifest, TrashPolicy,
        read_trash_entry, write_trash_entry,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrashPlanRequest {
    pub skill_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrashEntryRequest {
    pub entry_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrashExecuteRequest {
    pub operation_id: String,
    pub plan_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermanentDeleteRequest {
    pub entry_id: String,
    pub confirmation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashBlockerView {
    pub code: String,
    pub detail: String,
    pub deployment_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntryView {
    pub entry_id: String,
    pub skill_id: String,
    pub display_name: String,
    pub original_working_path: String,
    pub trashed_at: String,
    pub retention_deadline: Option<String>,
    pub retention_policy: String,
    pub protected_references: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashPlanView {
    pub operation_id: String,
    pub plan_digest: String,
    pub entry: TrashEntryView,
    pub blockers: Vec<TrashBlockerView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashExecutionView {
    pub operation_id: String,
    pub outcome: String,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrashRetentionSummary {
    pub total_entries: u32,
    pub expired_entries: u32,
    pub protected_entries: u32,
    pub next_deadline: Option<String>,
}

fn automatic_cleanup_eligible(
    policy: TrashPolicy,
    deadline: Option<UtcTimestamp>,
    now: UtcTimestamp,
) -> bool {
    !policy.never() && deadline.is_some_and(|value| value <= now)
}

#[derive(Debug, Error)]
pub enum TrashError {
    #[error("invalid identifier: {0}")]
    InvalidId(String),
    #[error("Skill was not found")]
    Missing,
    #[error("only an active indexed Skill can be moved to Trash")]
    NotActive,
    #[error("Trash operation failed: {0}")]
    Operation(#[from] OperationError),
    #[error("repository failed: {0}")]
    Repository(#[from] RepositoryError),
    #[error("Trash evidence failed: {0}")]
    Evidence(String),
}

pub struct TrashService {
    vault: Arc<OpenVault>,
    coordinator: Arc<OperationCoordinator>,
    store: OperationStore,
}

impl TrashService {
    pub fn with_runtime(
        vault: Arc<OpenVault>,
        coordinator: Arc<OperationCoordinator>,
    ) -> Result<Self, TrashError> {
        let store = OperationStore::open(vault.paths.manager())
            .map_err(|e| TrashError::Evidence(e.to_string()))?;
        Ok(Self {
            vault,
            coordinator,
            store,
        })
    }

    /// Returns blockers without creating an operation. A reviewable plan is persisted only after
    /// every deployment relationship is inactive.
    pub fn plan_move_to_trash(
        &self,
        request: &TrashPlanRequest,
    ) -> Result<TrashPlanView, TrashError> {
        let skill_id = SkillId::from_str(&request.skill_id)
            .map_err(|e| TrashError::InvalidId(e.to_string()))?;
        let skill = self
            .vault
            .repositories
            .skill(skill_id)?
            .ok_or(TrashError::Missing)?;
        if skill.lifecycle != SkillLifecycle::Active {
            return Err(TrashError::NotActive);
        }
        let active: Vec<_> = self
            .vault
            .repositories
            .skill_deployments(skill_id)?
            .into_iter()
            .filter(|d| d.active)
            .collect();
        if !active.is_empty() {
            return Ok(TrashPlanView {
                operation_id: String::new(),
                plan_digest: String::new(),
                entry: entry_view(&skill, TrashEntryId::generate(), UtcTimestamp::now(), &[]),
                blockers: vec![TrashBlockerView {
                    code: "active_deployments".into(),
                    detail: "Undeploy every active deployment before moving this Skill to Trash."
                        .into(),
                    deployment_ids: active.iter().map(|d| d.id.to_string()).collect(),
                }],
            });
        }
        let operation_id = OperationId::generate();
        let entry_id = TrashEntryId::generate();
        let now = UtcTimestamp::now();
        let builder = TrashBuilder {
            vault: Arc::clone(&self.vault),
            skill: skill.clone(),
            entry_id,
            now,
        };
        let intent = OperationIntent {
            operation_id,
            kind: OperationKind::MoveToTrash,
            selected_skill_ids: vec![skill_id],
            selected_target_ids: vec![],
            selected_deployment_ids: vec![],
            ownership_choices: vec![],
        };
        let plan = OperationPlanner::new(self.store.clone()).plan(
            &intent,
            &builder,
            &CancellationToken::default(),
        )?;
        let c = plan.content.trash.as_ref().expect("Trash builder context");
        Ok(TrashPlanView {
            operation_id: operation_id.to_string(),
            plan_digest: plan.plan_digest.to_string(),
            entry: entry_view(
                &skill,
                entry_id,
                c.retention_deadline.expect("30 day retention"),
                &c.protected_reference_ids,
            ),
            blockers: vec![],
        })
    }

    pub fn execute_move_to_trash(
        &self,
        request: &TrashExecuteRequest,
    ) -> Result<TrashExecutionView, TrashError> {
        let id = OperationId::from_str(&request.operation_id)
            .map_err(|e| TrashError::InvalidId(e.to_string()))?;
        let stored = self
            .store
            .load(id)
            .map_err(|e| TrashError::Evidence(e.to_string()))?;
        if stored.plan.plan_digest.to_string() != request.plan_digest {
            return Err(TrashError::Evidence(
                "plan digest differs from reviewed plan".into(),
            ));
        }
        let execution = self.executor(&stored.plan)?.execute(
            id,
            stored.plan.plan_digest,
            &CancellationToken::default(),
        )?;
        Ok(TrashExecutionView {
            operation_id: id.to_string(),
            outcome: format!("{:?}", execution.outcome).to_lowercase(),
            replayed: execution.replayed,
        })
    }

    /// Plans restoration from exact, self-contained Trash evidence.
    pub fn plan_restore(&self, request: &TrashEntryRequest) -> Result<TrashPlanView, TrashError> {
        let entry_id = TrashEntryId::from_str(&request.entry_id)
            .map_err(|e| TrashError::InvalidId(e.to_string()))?;
        let skill = self
            .vault
            .repositories
            .skills()?
            .into_iter()
            .find(|skill| {
                skill.lifecycle == SkillLifecycle::Trashed
                    && self
                        .vault
                        .paths
                        .trash_entry_manifest(skill.id, entry_id)
                        .is_file()
            })
            .ok_or(TrashError::Missing)?;
        let entry_path = self.vault.paths.trash_entry(skill.id, entry_id);
        let manifest = read_trash_entry(&self.vault.paths.trash_entry_manifest(skill.id, entry_id))
            .map_err(|e| TrashError::Evidence(e.to_string()))?;
        if manifest.entry_id != entry_id
            || manifest.skill_id != skill.id
            || manifest.working_digest != skill.working_digest
            || manifest.baseline_digest != skill.baseline_digest
        {
            return Err(TrashError::Evidence(
                "Trash manifest differs from indexed Skill".into(),
            ));
        }
        let bundle = entry_path
            .join("working")
            .join(manifest.skill_manifest.deployment_name.as_str());
        if hash_bundle(&bundle, BundleCaps::default())
            .map_err(|e| TrashError::Evidence(e.to_string()))?
            .digest
            != manifest.working_digest
        {
            return Err(TrashError::Evidence(
                "trashed working tree digest mismatch".into(),
            ));
        }
        let original_container = self
            .vault
            .paths
            .root()
            .join(manifest.original_working_path.as_str())
            .parent()
            .ok_or_else(|| TrashError::Evidence("invalid original working path".into()))?
            .to_path_buf();
        let container_id = if fs::symlink_metadata(&original_container).is_err() {
            skill.id
        } else {
            SkillId::generate()
        };
        let destination = BundleRelativePath::parse(&format!("skills/{container_id}"))
            .map_err(|e| TrashError::Evidence(e.to_string()))?;
        let operation_id = OperationId::generate();
        let now = UtcTimestamp::now();
        let builder = RestoreBuilder {
            vault: Arc::clone(&self.vault),
            skill: skill.clone(),
            manifest,
            destination,
            now,
        };
        let intent = OperationIntent {
            operation_id,
            kind: OperationKind::Restore,
            selected_skill_ids: vec![skill.id],
            selected_target_ids: vec![],
            selected_deployment_ids: vec![],
            ownership_choices: vec![],
        };
        let plan = OperationPlanner::new(self.store.clone()).plan(
            &intent,
            &builder,
            &CancellationToken::default(),
        )?;
        Ok(TrashPlanView {
            operation_id: operation_id.to_string(),
            plan_digest: plan.plan_digest.to_string(),
            entry: entry_view(&skill, entry_id, now, &[]),
            blockers: vec![],
        })
    }

    pub fn execute_restore(
        &self,
        request: &TrashExecuteRequest,
    ) -> Result<TrashExecutionView, TrashError> {
        self.execute_checked(request, OperationKind::Restore)
    }

    /// Plans a manual permanent delete. Retention controls automatic cleanup only; exact
    /// secondary confirmation permits manual deletion, but never bypasses durable references.
    pub fn plan_permanent_delete(
        &self,
        request: &PermanentDeleteRequest,
    ) -> Result<TrashPlanView, TrashError> {
        let entry_id = TrashEntryId::from_str(&request.entry_id)
            .map_err(|e| TrashError::InvalidId(e.to_string()))?;
        let skill = self
            .vault
            .repositories
            .skills()?
            .into_iter()
            .find(|s| {
                s.lifecycle == SkillLifecycle::Trashed
                    && self
                        .vault
                        .paths
                        .trash_entry_manifest(s.id, entry_id)
                        .is_file()
            })
            .ok_or(TrashError::Missing)?;
        let entry = self.vault.paths.trash_entry(skill.id, entry_id);
        let manifest = read_trash_entry(&self.vault.paths.trash_entry_manifest(skill.id, entry_id))
            .map_err(|e| TrashError::Evidence(e.to_string()))?;
        validate_entry(&skill, entry_id, &entry, &manifest)?;
        if request.confirmation != manifest.skill_manifest.display_name {
            return Err(TrashError::Evidence(
                "secondary confirmation does not exactly match the Skill display name".into(),
            ));
        }
        let blockers = self
            .vault
            .repositories
            .permanent_delete_blockers(skill.id, manifest.working_digest)?;
        if !blockers.is_empty() {
            return Ok(TrashPlanView { operation_id: String::new(), plan_digest: String::new(), entry: manifest_view(&manifest), blockers: vec![TrashBlockerView { code: "protected_or_unresolved_references".into(), detail: "Protected Snapshot or unresolved operation evidence still refers to this Trash entry.".into(), deployment_ids: blockers }] });
        }
        // Nonterminal file journals are authoritative even if their projection has not landed.
        for id in self
            .store
            .nonterminal_operation_ids()
            .map_err(|e| TrashError::Evidence(e.to_string()))?
        {
            let stored = self
                .store
                .load(id)
                .map_err(|e| TrashError::Evidence(e.to_string()))?;
            if stored.plan.content.selected_skill_ids.contains(&skill.id) {
                return Ok(TrashPlanView {
                    operation_id: String::new(),
                    plan_digest: String::new(),
                    entry: manifest_view(&manifest),
                    blockers: vec![TrashBlockerView {
                        code: "unresolved_journal".into(),
                        detail: "An unresolved operation journal refers to this Skill.".into(),
                        deployment_ids: vec![id.to_string()],
                    }],
                });
            }
        }
        let operation_id = OperationId::generate();
        let builder = PermanentDeleteBuilder {
            vault: Arc::clone(&self.vault),
            skill: skill.clone(),
            manifest: manifest.clone(),
            now: UtcTimestamp::now(),
        };
        let intent = OperationIntent {
            operation_id,
            kind: OperationKind::PermanentlyDelete,
            selected_skill_ids: vec![skill.id],
            selected_target_ids: vec![],
            selected_deployment_ids: vec![],
            ownership_choices: vec![],
        };
        let plan = OperationPlanner::new(self.store.clone()).plan(
            &intent,
            &builder,
            &CancellationToken::default(),
        )?;
        Ok(TrashPlanView {
            operation_id: operation_id.to_string(),
            plan_digest: plan.plan_digest.to_string(),
            entry: manifest_view(&manifest),
            blockers: vec![],
        })
    }

    pub fn execute_permanent_delete(
        &self,
        request: &TrashExecuteRequest,
    ) -> Result<TrashExecutionView, TrashError> {
        self.execute_checked(request, OperationKind::PermanentlyDelete)
    }

    pub fn retention_summary(
        &self,
        now: UtcTimestamp,
    ) -> Result<TrashRetentionSummary, TrashError> {
        let mut total = 0;
        let mut expired = 0;
        let mut protected = 0;
        let mut next: Option<UtcTimestamp> = None;
        for skill in self
            .vault
            .repositories
            .skills()?
            .into_iter()
            .filter(|s| s.lifecycle == SkillLifecycle::Trashed)
        {
            let parent = self
                .vault
                .paths
                .trash_entry(skill.id, TrashEntryId::generate())
                .parent()
                .expect("entry parent")
                .to_path_buf();
            for item in fs::read_dir(parent).into_iter().flatten().flatten() {
                let Ok(entry_id) = item.file_name().to_string_lossy().parse() else {
                    continue;
                };
                if let Ok(m) =
                    read_trash_entry(&self.vault.paths.trash_entry_manifest(skill.id, entry_id))
                {
                    total += 1;
                    if !m.protected_references.is_empty() {
                        protected += 1;
                    }
                    if let Some(d) = m.retention_deadline {
                        if automatic_cleanup_eligible(m.retention_policy, Some(d), now) {
                            expired += 1;
                        } else if next.is_none_or(|n| d < n) {
                            next = Some(d);
                        }
                    }
                }
            }
        }
        Ok(TrashRetentionSummary {
            total_entries: total,
            expired_entries: expired,
            protected_entries: protected,
            next_deadline: next.map(|d| d.to_string()),
        })
    }

    /// Lists validated Trash manifests. Invalid or orphaned entries are not presented as safe
    /// mutation targets; Vault verification remains responsible for reporting them.
    pub fn entries(&self) -> Result<Vec<TrashEntryView>, TrashError> {
        let mut entries = Vec::new();
        for skill in self
            .vault
            .repositories
            .skills()?
            .into_iter()
            .filter(|skill| skill.lifecycle == SkillLifecycle::Trashed)
        {
            let parent = self
                .vault
                .paths
                .trash_entry(skill.id, TrashEntryId::generate())
                .parent()
                .expect("Trash entry parent")
                .to_path_buf();
            for item in fs::read_dir(parent).into_iter().flatten().flatten() {
                let Ok(entry_id) = item.file_name().to_string_lossy().parse() else {
                    continue;
                };
                if let Ok(manifest) =
                    read_trash_entry(&self.vault.paths.trash_entry_manifest(skill.id, entry_id))
                    && manifest.skill_id == skill.id
                {
                    entries.push(manifest_view(&manifest));
                }
            }
        }
        entries.sort_by(|left, right| right.trashed_at.cmp(&left.trashed_at));
        Ok(entries)
    }

    pub fn entry(&self, request: &TrashEntryRequest) -> Result<TrashEntryView, TrashError> {
        let entry_id = TrashEntryId::from_str(&request.entry_id)
            .map_err(|error| TrashError::InvalidId(error.to_string()))?;
        self.entries()?
            .into_iter()
            .find(|entry| entry.entry_id == entry_id.to_string())
            .ok_or(TrashError::Missing)
    }

    pub fn recover_operation(
        &self,
        id: OperationId,
    ) -> Result<crate::operations::OperationExecution, TrashError> {
        let plan = self
            .store
            .load(id)
            .map_err(|e| TrashError::Evidence(e.to_string()))?
            .plan;
        Ok(self.executor(&plan)?.recover(id)?)
    }

    fn executor(&self, plan: &OperationPlan) -> Result<OperationExecutor, TrashError> {
        if !matches!(
            plan.content.kind,
            OperationKind::MoveToTrash | OperationKind::Restore | OperationKind::PermanentlyDelete
        ) || plan.content.trash.is_none()
        {
            return Err(TrashError::Evidence(
                "operation is not a supported Trash transition".into(),
            ));
        }
        let mut roots = TargetRoots::new();
        for step in &plan.content.steps {
            roots.insert(
                step.path.target_id(),
                AuthorizedRoot::open(self.vault.paths.root())
                    .map_err(|e| TrashError::Evidence(e.to_string()))?,
            );
        }
        let hooks = Arc::new(TrashHooks {
            vault: Arc::clone(&self.vault),
        });
        Ok(OperationExecutor::new(
            self.store.clone(),
            Arc::clone(&self.coordinator),
            roots,
            hooks.clone(),
            hooks.clone(),
            hooks,
        ))
    }

    fn execute_checked(
        &self,
        request: &TrashExecuteRequest,
        kind: OperationKind,
    ) -> Result<TrashExecutionView, TrashError> {
        let id = OperationId::from_str(&request.operation_id)
            .map_err(|e| TrashError::InvalidId(e.to_string()))?;
        let stored = self
            .store
            .load(id)
            .map_err(|e| TrashError::Evidence(e.to_string()))?;
        if stored.plan.content.kind != kind
            || stored.plan.plan_digest.to_string() != request.plan_digest
        {
            return Err(TrashError::Evidence(
                "plan differs from reviewed operation".into(),
            ));
        }
        let execution = self.executor(&stored.plan)?.execute(
            id,
            stored.plan.plan_digest,
            &CancellationToken::default(),
        )?;
        Ok(TrashExecutionView {
            operation_id: id.to_string(),
            outcome: format!("{:?}", execution.outcome).to_lowercase(),
            replayed: execution.replayed,
        })
    }
}

fn validate_entry(
    skill: &SkillRecord,
    entry_id: TrashEntryId,
    entry: &Path,
    manifest: &TrashEntryManifest,
) -> Result<(), TrashError> {
    if manifest.entry_id != entry_id
        || manifest.skill_id != skill.id
        || manifest.working_digest != skill.working_digest
        || manifest.baseline_digest != skill.baseline_digest
        || manifest.skill_manifest.display_name != skill.display_name
    {
        return Err(TrashError::Evidence(
            "Trash manifest differs from indexed Skill".into(),
        ));
    }
    let bundle = entry
        .join("working")
        .join(manifest.skill_manifest.deployment_name.as_str());
    if hash_bundle(&bundle, BundleCaps::default())
        .map_err(|e| TrashError::Evidence(e.to_string()))?
        .digest
        != manifest.working_digest
    {
        return Err(TrashError::Evidence(
            "trashed working tree digest mismatch".into(),
        ));
    }
    Ok(())
}

fn manifest_view(m: &TrashEntryManifest) -> TrashEntryView {
    TrashEntryView {
        entry_id: m.entry_id.to_string(),
        skill_id: m.skill_id.to_string(),
        display_name: m.skill_manifest.display_name.clone(),
        original_working_path: m.original_working_path.to_string(),
        trashed_at: m.trashed_at.to_string(),
        retention_deadline: m.retention_deadline.map(|d| d.to_string()),
        retention_policy: if m.retention_policy.never() {
            "never"
        } else {
            "retain_30_days"
        }
        .into(),
        protected_references: m.protected_references.clone(),
    }
}

struct PermanentDeleteBuilder {
    vault: Arc<OpenVault>,
    skill: SkillRecord,
    manifest: TrashEntryManifest,
    now: UtcTimestamp,
}
impl PlanBuilder for PermanentDeleteBuilder {
    #[allow(clippy::too_many_lines, clippy::default_trait_access)]
    fn build_content(
        &self,
        intent: &OperationIntent,
        _: &CancellationToken,
    ) -> Result<OperationPlanContent, OperationError> {
        let root = AuthorizedRoot::open(self.vault.paths.root())
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let rel = BundleRelativePath::parse(&format!(".manager/trash/{}", self.manifest.entry_id))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let source = root
            .authorize(&rel)
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let adapter = AdapterId::from_str("skills-hub@1")
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let before = PathFingerprint {
            expected_kind: EntryKind::Directory,
            raw_symlink_target: None,
            metadata: Some(MetadataFingerprint::from_metadata(
                &fs::symlink_metadata(source.path()).map_err(|e| OperationError::Filesystem {
                    context: "inspecting Trash entry",
                    source: e,
                })?,
            )),
            bundle_digest: Some(self.manifest.working_digest),
            bundle_subpath: Some(
                BundleRelativePath::parse(&format!(
                    "working/{}",
                    self.manifest.skill_manifest.deployment_name
                ))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            ),
            resolved_bundle_digest: None,
            managed_skill_id: Some(self.skill.id),
            managed_deployment_id: None,
            captured_at: self.now,
            adapter_id: adapter.clone(),
        };
        let absent = PathFingerprint {
            expected_kind: EntryKind::Absent,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: None,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: None,
            managed_deployment_id: None,
            captured_at: self.now,
            adapter_id: adapter,
        };
        let step = PlanStep::new(
            PlanAction::Remove,
            PlanPath::from_authorized(TargetId::generate(), &source)
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            None,
            None,
            before,
            absent,
            true,
        );
        let context = TrashPlanContext {
            action: TrashAction::PermanentlyDelete,
            skill_id: self.skill.id,
            display_name: self.manifest.skill_manifest.display_name.clone(),
            deployment_name: self.manifest.skill_manifest.deployment_name.clone(),
            lifecycle_before: SkillLifecycle::Trashed,
            lifecycle_after: SkillLifecycle::PermanentlyRemoved,
            trash_entry_id: self.manifest.entry_id,
            source_relative_path: rel,
            destination_relative_path: None,
            skill_manifest_path: BundleRelativePath::parse(&format!(
                ".manager/manifests/skills/{}.json",
                self.skill.id
            ))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            provenance_paths: vec![],
            working_digest: self.manifest.working_digest,
            baseline_digest: self.manifest.baseline_digest,
            active_deployment_ids: vec![],
            deployments_resolved: true,
            retention_policy: if self.manifest.retention_policy.never() {
                TrashRetentionPolicy::Never
            } else {
                TrashRetentionPolicy::Days30
            },
            retention_deadline: self.manifest.retention_deadline,
            confirmation_subject: self.manifest.skill_manifest.display_name.clone(),
            protected_reference_ids: vec![],
            source_step_order: 0,
            destination_step_order: None,
        };
        Ok(OperationPlanContent::new(
            intent.operation_id,
            intent.kind,
            self.now,
            self.now
                .checked_add(DurationMillis(300_000))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            vec![self.skill.id],
            vec![],
            vec![],
            vec![],
            BundleCaps::default(),
            Default::default(),
            vec![step],
            vec![],
            RecoverySummary {
                snapshot_count: 1,
                estimated_staging_bytes: 0,
                estimated_snapshot_bytes: 0,
                estimated_rollback_bytes: 0,
                spans_filesystems: false,
            },
            vec![],
        )
        .with_trash_context(context))
    }
}

struct RestoreBuilder {
    vault: Arc<OpenVault>,
    skill: SkillRecord,
    manifest: TrashEntryManifest,
    destination: BundleRelativePath,
    now: UtcTimestamp,
}
impl PlanBuilder for RestoreBuilder {
    #[allow(clippy::too_many_lines, clippy::default_trait_access)]
    fn build_content(
        &self,
        intent: &OperationIntent,
        _: &CancellationToken,
    ) -> Result<OperationPlanContent, OperationError> {
        let root = AuthorizedRoot::open(self.vault.paths.root())
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let source_rel =
            BundleRelativePath::parse(&format!(".manager/trash/{}", self.manifest.entry_id))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let source = root
            .authorize(&source_rel)
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let dest = root
            .authorize(&self.destination)
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let adapter = AdapterId::from_str("skills-hub@1")
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let absent = PathFingerprint {
            expected_kind: EntryKind::Absent,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: None,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: None,
            managed_deployment_id: None,
            captured_at: self.now,
            adapter_id: adapter.clone(),
        };
        let before = PathFingerprint {
            expected_kind: EntryKind::Directory,
            raw_symlink_target: None,
            metadata: Some(MetadataFingerprint::from_metadata(
                &fs::symlink_metadata(source.path()).map_err(|e| OperationError::Filesystem {
                    context: "inspecting Trash entry",
                    source: e,
                })?,
            )),
            bundle_digest: Some(self.manifest.working_digest),
            bundle_subpath: Some(
                BundleRelativePath::parse(&format!(
                    "working/{}",
                    self.manifest.skill_manifest.deployment_name.as_str()
                ))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            ),
            resolved_bundle_digest: None,
            managed_skill_id: Some(self.skill.id),
            managed_deployment_id: None,
            captured_at: self.now,
            adapter_id: adapter.clone(),
        };
        let after = PathFingerprint {
            expected_kind: EntryKind::Directory,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: Some(self.manifest.working_digest),
            bundle_subpath: Some(
                BundleRelativePath::parse(self.manifest.skill_manifest.deployment_name.as_str())
                    .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            ),
            resolved_bundle_digest: None,
            managed_skill_id: Some(self.skill.id),
            managed_deployment_id: None,
            captured_at: self.now,
            adapter_id: adapter,
        };
        let steps = vec![
            PlanStep::new(
                PlanAction::Remove,
                PlanPath::from_authorized(TargetId::generate(), &source)
                    .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
                None,
                None,
                before,
                absent.clone(),
                true,
            ),
            PlanStep::new(
                PlanAction::Create,
                PlanPath::from_authorized(TargetId::generate(), &dest)
                    .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
                None,
                None,
                absent,
                after,
                false,
            ),
        ];
        let c = TrashPlanContext {
            action: TrashAction::Restore,
            skill_id: self.skill.id,
            display_name: self.manifest.skill_manifest.display_name.clone(),
            deployment_name: self.manifest.skill_manifest.deployment_name.clone(),
            lifecycle_before: SkillLifecycle::Trashed,
            lifecycle_after: SkillLifecycle::Active,
            trash_entry_id: self.manifest.entry_id,
            source_relative_path: source_rel,
            destination_relative_path: Some(self.destination.clone()),
            skill_manifest_path: BundleRelativePath::parse(&format!(
                ".manager/manifests/skills/{}.json",
                self.skill.id
            ))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            provenance_paths: vec![],
            working_digest: self.manifest.working_digest,
            baseline_digest: self.manifest.baseline_digest,
            active_deployment_ids: vec![],
            deployments_resolved: true,
            retention_policy: if self.manifest.retention_policy.never() {
                TrashRetentionPolicy::Never
            } else {
                TrashRetentionPolicy::Days30
            },
            retention_deadline: self.manifest.retention_deadline,
            confirmation_subject: self.manifest.skill_manifest.display_name.clone(),
            protected_reference_ids: self.manifest.protected_references.clone(),
            source_step_order: 0,
            destination_step_order: Some(1),
        };
        Ok(OperationPlanContent::new(
            intent.operation_id,
            intent.kind,
            self.now,
            self.now
                .checked_add(DurationMillis(300_000))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            vec![self.skill.id],
            vec![],
            vec![],
            vec![],
            BundleCaps::default(),
            Default::default(),
            steps,
            vec![],
            RecoverySummary {
                snapshot_count: 1,
                estimated_staging_bytes: 0,
                estimated_snapshot_bytes: 0,
                estimated_rollback_bytes: 0,
                spans_filesystems: false,
            },
            vec![],
        )
        .with_trash_context(c))
    }
}

fn entry_view(
    skill: &SkillRecord,
    id: TrashEntryId,
    at: UtcTimestamp,
    refs: &[String],
) -> TrashEntryView {
    TrashEntryView {
        entry_id: id.to_string(),
        skill_id: skill.id.to_string(),
        display_name: skill.display_name.clone(),
        original_working_path: skill.working_path.to_string(),
        trashed_at: at.to_string(),
        retention_deadline: Some(at.to_string()),
        retention_policy: "retain_30_days".into(),
        protected_references: refs.to_vec(),
    }
}

struct TrashBuilder {
    vault: Arc<OpenVault>,
    skill: SkillRecord,
    entry_id: TrashEntryId,
    now: UtcTimestamp,
}
impl PlanBuilder for TrashBuilder {
    #[allow(clippy::too_many_lines, clippy::default_trait_access)]
    fn build_content(
        &self,
        intent: &OperationIntent,
        cancellation: &CancellationToken,
    ) -> Result<OperationPlanContent, OperationError> {
        cancellation.check()?;
        let root = AuthorizedRoot::open(self.vault.paths.root())
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let source_rel = BundleRelativePath::parse(&format!("skills/{}", self.skill.id))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let dest_rel = BundleRelativePath::parse(&format!(".manager/trash/{}", self.entry_id))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let source = root
            .authorize(&source_rel)
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let dest = root
            .authorize(&dest_rel)
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let metadata = MetadataFingerprint::from_metadata(
            &fs::symlink_metadata(source.path()).map_err(|e| OperationError::Filesystem {
                context: "inspecting Skill container",
                source: e,
            })?,
        );
        let adapter = AdapterId::from_str("skills-hub@1")
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let before = PathFingerprint {
            expected_kind: EntryKind::Directory,
            raw_symlink_target: None,
            metadata: Some(metadata),
            bundle_digest: Some(self.skill.working_digest),
            bundle_subpath: Some(
                BundleRelativePath::parse(self.skill.deployment_name.as_str())
                    .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            ),
            resolved_bundle_digest: None,
            managed_skill_id: Some(self.skill.id),
            managed_deployment_id: None,
            captured_at: self.now,
            adapter_id: adapter.clone(),
        };
        let absent = PathFingerprint {
            expected_kind: EntryKind::Absent,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: None,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: None,
            managed_deployment_id: None,
            captured_at: self.now,
            adapter_id: adapter.clone(),
        };
        let mut after = before.clone();
        after.metadata = None;
        after.bundle_subpath = Some(
            BundleRelativePath::parse(&format!("working/{}", self.skill.deployment_name))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
        );
        let steps = vec![
            PlanStep::new(
                PlanAction::Remove,
                PlanPath::from_authorized(TargetId::generate(), &source)
                    .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
                None,
                None,
                before,
                absent.clone(),
                true,
            ),
            PlanStep::new(
                PlanAction::Create,
                PlanPath::from_authorized(TargetId::generate(), &dest)
                    .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
                None,
                None,
                absent,
                after,
                false,
            ),
        ];
        let deadline = self
            .now
            .checked_add(DurationMillis(30 * 24 * 60 * 60 * 1000))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?;
        let context = TrashPlanContext {
            action: TrashAction::MoveToTrash,
            skill_id: self.skill.id,
            display_name: self.skill.display_name.clone(),
            deployment_name: self.skill.deployment_name.clone(),
            lifecycle_before: SkillLifecycle::Active,
            lifecycle_after: SkillLifecycle::Trashed,
            trash_entry_id: self.entry_id,
            source_relative_path: source_rel,
            destination_relative_path: Some(dest_rel),
            skill_manifest_path: BundleRelativePath::parse(&format!(
                ".manager/manifests/skills/{}.json",
                self.skill.id
            ))
            .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            provenance_paths: vec![],
            working_digest: self.skill.working_digest,
            baseline_digest: self.skill.baseline_digest,
            active_deployment_ids: vec![],
            deployments_resolved: true,
            retention_policy: TrashRetentionPolicy::Days30,
            retention_deadline: Some(deadline),
            confirmation_subject: self.skill.display_name.clone(),
            protected_reference_ids: vec![format!("object:{}", self.skill.working_digest)],
            source_step_order: 0,
            destination_step_order: Some(1),
        };
        Ok(OperationPlanContent::new(
            intent.operation_id,
            intent.kind,
            self.now,
            self.now
                .checked_add(DurationMillis(300_000))
                .map_err(|e| OperationError::InvalidPlan(e.to_string()))?,
            intent.selected_skill_ids.clone(),
            vec![],
            vec![],
            vec![],
            BundleCaps::default(),
            Default::default(),
            steps,
            vec![],
            RecoverySummary {
                snapshot_count: 1,
                estimated_staging_bytes: 0,
                estimated_snapshot_bytes: 0,
                estimated_rollback_bytes: 0,
                spans_filesystems: false,
            },
            vec![],
        )
        .with_trash_context(context))
    }
}

struct TrashHooks {
    vault: Arc<OpenVault>,
}
#[allow(clippy::needless_pass_by_value)]
fn hook(e: impl ToString) -> OperationHookError {
    OperationHookError::new(e.to_string())
}
impl StagingProvider for TrashHooks {
    fn stage(
        &self,
        plan: &OperationPlan,
        step: &PlanStep,
        staging: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), OperationHookError> {
        cancellation.check().map_err(hook)?;
        let c = plan
            .content
            .trash
            .as_ref()
            .ok_or_else(|| hook("missing Trash context"))?;
        if c.action == TrashAction::PermanentlyDelete {
            return Err(hook("permanent delete has no staged create step"));
        }
        if step.order != c.destination_step_order.expect("destination") {
            return Err(hook("unexpected staged Trash step"));
        }
        fs::create_dir(staging).map_err(hook)?;
        let source = self
            .vault
            .paths
            .root()
            .join(c.source_relative_path.as_str());
        let (copy_source, copy_destination) = if c.action == TrashAction::Restore {
            (
                source.join("working").join(c.deployment_name.as_str()),
                staging.join(c.deployment_name.as_str()),
            )
        } else {
            fs::create_dir(staging.join("working")).map_err(hook)?;
            (
                source.join(c.deployment_name.as_str()),
                staging.join("working").join(c.deployment_name.as_str()),
            )
        };
        let copied = copy_bundle_exact(&copy_source, &copy_destination, plan.content.bundle_caps)
            .map_err(hook)?;
        if copied.digest != c.working_digest {
            return Err(hook("staged Trash digest mismatch"));
        }
        Ok(())
    }
}
impl SnapshotRegistrar for TrashHooks {
    fn register(
        &self,
        plan: &OperationPlan,
        steps: &[PlanStep],
        cancellation: &CancellationToken,
    ) -> Result<SnapshotRegistration, OperationHookError> {
        let c = plan
            .content
            .trash
            .as_ref()
            .ok_or_else(|| hook("missing Trash context"))?;
        let step = steps
            .first()
            .ok_or_else(|| hook("missing protected source"))?;
        cancellation.check().map_err(hook)?;
        let snapshot_path = if c.action == TrashAction::Restore {
            Path::new(step.path.display_path())
                .join("working")
                .join(c.deployment_name.as_str())
        } else if c.action == TrashAction::MoveToTrash {
            Path::new(step.path.display_path()).join(c.deployment_name.as_str())
        } else {
            Path::new(step.path.display_path())
                .join("working")
                .join(c.deployment_name.as_str())
        };
        let publication = self
            .vault
            .objects
            .publish(
                plan.content.operation_id,
                &snapshot_path,
                Some(c.working_digest),
                UtcTimestamp::now(),
            )
            .map_err(hook)?;
        self.vault
            .repositories
            .upsert_object(ObjectRecord {
                digest: c.working_digest,
                relative_path: publication
                    .path
                    .strip_prefix(self.vault.paths.root())
                    .map_err(hook)?
                    .to_string_lossy()
                    .parse()
                    .map_err(hook)?,
                entry_count: publication.manifest.entry_count,
                byte_count: publication.manifest.byte_count,
                verified_at: UtcTimestamp::now(),
            })
            .map_err(hook)?;
        Ok(SnapshotRegistration {
            protections: vec![SnapshotProtection {
                step_order: step.order,
                reference: format!("object:{}", c.working_digest),
                before: step.before.clone(),
            }],
        })
    }
}
impl OperationFinalizer for TrashHooks {
    fn publish_manifests(
        &self,
        plan: &OperationPlan,
        _: &crate::operations::OperationJournal,
    ) -> Result<(), OperationHookError> {
        let c = plan
            .content
            .trash
            .as_ref()
            .ok_or_else(|| hook("missing Trash context"))?;
        if c.action == TrashAction::PermanentlyDelete {
            let path = self
                .vault
                .paths
                .trash_entry_manifest(c.skill_id, c.trash_entry_id);
            if path.exists() {
                fs::remove_file(path).map_err(hook)?;
            }
            return Ok(());
        }
        if c.action == TrashAction::Restore {
            let destination = self.vault.paths.root().join(
                c.destination_relative_path
                    .as_ref()
                    .expect("destination")
                    .as_str(),
            );
            let bundle = destination.join(c.deployment_name.as_str());
            if hash_bundle(&bundle, plan.content.bundle_caps)
                .map_err(hook)?
                .digest
                != c.working_digest
            {
                return Err(hook("restored working digest mismatch"));
            }
            let mut skill_manifest = self.vault.manifests.read_skill(c.skill_id).map_err(hook)?;
            if skill_manifest.skill_id != c.skill_id
                || skill_manifest.working_digest != c.working_digest
                || skill_manifest.baseline_digest != c.baseline_digest
            {
                return Err(hook("Restore ownership evidence mismatch"));
            }
            skill_manifest.working_path = BundleRelativePath::parse(&format!(
                "{}/{}",
                c.destination_relative_path.as_ref().expect("destination"),
                c.deployment_name.as_str()
            ))
            .map_err(hook)?;
            self.vault
                .manifests
                .write_skill(&skill_manifest)
                .map_err(hook)?;
            if self.vault.manifests.read_skill(c.skill_id).map_err(hook)? != skill_manifest {
                return Err(hook("restored Skill manifest verification mismatch"));
            }
            return Ok(());
        }
        let entry = self.vault.paths.root().join(
            c.destination_relative_path
                .as_ref()
                .expect("destination")
                .as_str(),
        );
        let bundle = entry.join("working").join(c.deployment_name.as_str());
        if hash_bundle(&bundle, plan.content.bundle_caps)
            .map_err(hook)?
            .digest
            != c.working_digest
        {
            return Err(hook("Trash working digest mismatch"));
        }
        let skill_manifest = self.vault.manifests.read_skill(c.skill_id).map_err(hook)?;
        let manifest = TrashEntryManifest {
            schema_version: 1,
            entry_id: c.trash_entry_id,
            skill_id: c.skill_id,
            original_working_path: skill_manifest.working_path.clone(),
            source_provenance: skill_manifest.sources.clone(),
            skill_manifest,
            working_digest: c.working_digest,
            baseline_digest: c.baseline_digest,
            trashed_at: plan.content.created_at,
            retention_policy: TrashPolicy::Retain30Days,
            retention_deadline: c.retention_deadline,
            protected_references: c.protected_reference_ids.clone(),
        };
        let manifest_path = self
            .vault
            .paths
            .trash_entry_manifest(c.skill_id, c.trash_entry_id);
        write_trash_entry(&manifest_path, &manifest).map_err(hook)?;
        let checked = read_trash_entry(&manifest_path).map_err(hook)?;
        if checked != manifest {
            return Err(hook("Trash manifest verification mismatch"));
        }
        Ok(())
    }
    fn finalize_projection(
        &self,
        plan: &OperationPlan,
        journal: &crate::operations::OperationJournal,
    ) -> Result<(), OperationHookError> {
        let c = plan
            .content
            .trash
            .as_ref()
            .ok_or_else(|| hook("missing Trash context"))?;
        if c.action == TrashAction::PermanentlyDelete {
            return self
                .vault
                .repositories
                .finalize_permanent_delete(
                    c.skill_id,
                    plan.content.operation_id,
                    plan.plan_digest.to_string(),
                    SnapshotId::generate(),
                    ActivityId::generate(),
                    journal.updated_at,
                    BundleRelativePath::parse(&format!(
                        ".manager/operations/{}",
                        plan.content.operation_id
                    ))
                    .map_err(hook)?,
                    c.working_digest,
                )
                .map_err(hook);
        }
        if c.action == TrashAction::Restore {
            return self
                .vault
                .repositories
                .finalize_restore(
                    c.skill_id,
                    plan.content.operation_id,
                    plan.plan_digest.to_string(),
                    ActivityId::generate(),
                    journal.updated_at,
                    BundleRelativePath::parse(&format!(
                        ".manager/operations/{}",
                        plan.content.operation_id
                    ))
                    .map_err(hook)?,
                    BundleRelativePath::parse(&format!(
                        "{}/{}",
                        c.destination_relative_path.as_ref().expect("destination"),
                        c.deployment_name.as_str()
                    ))
                    .map_err(hook)?,
                    c.working_digest,
                )
                .map_err(hook);
        }
        self.vault
            .repositories
            .finalize_move_to_trash(
                c.skill_id,
                plan.content.operation_id,
                plan.plan_digest.to_string(),
                SnapshotId::generate(),
                ActivityId::generate(),
                journal.updated_at,
                BundleRelativePath::parse(&format!(
                    ".manager/operations/{}",
                    plan.content.operation_id
                ))
                .map_err(hook)?,
                c.working_digest,
            )
            .map_err(hook)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::{TempDir, tempdir};

    use crate::{
        domain::{DeploymentHealth, DeploymentId, DeploymentMode, DeploymentName},
        persistence::{
            DeploymentRecord, LocalSourceKind, SkillManifest, SkillManifestSource,
            SourceConfidence, TargetRecord,
        },
    };

    use super::*;

    struct Fixture {
        _temporary: TempDir,
        vault: Arc<OpenVault>,
        service: TrashService,
        skill_id: SkillId,
        working_container: PathBuf,
        working_bundle: PathBuf,
        source: PathBuf,
    }

    fn fixture() -> Fixture {
        let temporary = tempdir().unwrap();
        let vault = Arc::new(
            OpenVault::open(
                &temporary.path().join("vault"),
                &temporary.path().join("support"),
                &[],
            )
            .unwrap(),
        );
        let skill_id = SkillId::generate();
        let deployment_name = DeploymentName::parse("trash-fixture").unwrap();
        let relative: BundleRelativePath =
            format!("skills/{skill_id}/trash-fixture").parse().unwrap();
        let working_bundle = vault.paths.root().join(relative.as_str());
        let working_container = working_bundle.parent().unwrap().to_path_buf();
        fs::create_dir_all(working_bundle.join("nested")).unwrap();
        fs::write(working_bundle.join("SKILL.md"), b"trash fixture\n").unwrap();
        fs::write(working_bundle.join("nested/data.bin"), [0, 1, 255]).unwrap();
        let source = temporary.path().join("original/trash-fixture");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), b"source evidence\n").unwrap();
        let now = UtcTimestamp::now();
        let hashed = hash_bundle(&working_bundle, BundleCaps::default()).unwrap();
        vault
            .repositories
            .upsert_skill(SkillRecord {
                id: skill_id,
                display_name: "Trash Fixture".into(),
                deployment_name: deployment_name.clone(),
                working_path: relative,
                working_digest: hashed.digest,
                baseline_digest: hashed.digest,
                lifecycle: SkillLifecycle::Active,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        vault
            .manifests
            .write_skill(
                &SkillManifest::new(
                    skill_id,
                    "Trash Fixture".into(),
                    deployment_name,
                    hashed.digest,
                    hashed.digest,
                    now,
                    vec![SkillManifestSource {
                        kind: LocalSourceKind::LocalObservation,
                        path: source.clone(),
                        captured_at: now,
                        confidence: SourceConfidence::Observed,
                    }],
                )
                .unwrap(),
            )
            .unwrap();
        let service =
            TrashService::with_runtime(Arc::clone(&vault), Arc::new(OperationCoordinator::new()))
                .unwrap();
        Fixture {
            _temporary: temporary,
            vault,
            service,
            skill_id,
            working_container,
            working_bundle,
            source,
        }
    }

    fn tree_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries = fs::read_dir(path)
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &path, out);
                } else {
                    out.push((
                        path.strip_prefix(root).unwrap().into(),
                        fs::read(path).unwrap(),
                    ));
                }
            }
        }
        let mut result = Vec::new();
        visit(root, root, &mut result);
        result
    }

    fn move_to_trash(fixture: &Fixture) -> (TrashPlanView, TrashExecutionView) {
        let plan = fixture
            .service
            .plan_move_to_trash(&TrashPlanRequest {
                skill_id: fixture.skill_id.to_string(),
            })
            .unwrap();
        let result = fixture
            .service
            .execute_move_to_trash(&TrashExecuteRequest {
                operation_id: plan.operation_id.clone(),
                plan_digest: plan.plan_digest.clone(),
            })
            .unwrap();
        (plan, result)
    }

    #[test]
    fn deployed_skill_is_blocked_without_a_plan_or_any_write() {
        let fixture = fixture();
        let now = UtcTimestamp::now();
        let target_id = TargetId::generate();
        fixture
            .vault
            .repositories
            .upsert_target(TargetRecord {
                id: target_id,
                adapter_id: AdapterId::from_str("fixture@1").unwrap(),
                scope: "global".into(),
                root_path: fixture.source.clone(),
                canonical_root_path: fixture.source.clone(),
                project_id: None,
                is_override: false,
                is_custom: true,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        fixture
            .vault
            .repositories
            .upsert_deployment(DeploymentRecord {
                id: DeploymentId::generate(),
                skill_id: fixture.skill_id,
                target_id,
                deployment_name: DeploymentName::parse("trash-fixture").unwrap(),
                target_path: fixture.source.clone(),
                mode: DeploymentMode::ManagedCopy,
                expected_digest: fixture
                    .vault
                    .repositories
                    .skill(fixture.skill_id)
                    .unwrap()
                    .unwrap()
                    .working_digest,
                expected_link_target: None,
                health: DeploymentHealth::Clean,
                adapter_version: AdapterId::from_str("fixture@1").unwrap(),
                active: true,
                last_verified_at: Some(now),
                last_operation_id: None,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let before = tree_bytes(&fixture.working_bundle);
        let view = fixture
            .service
            .plan_move_to_trash(&TrashPlanRequest {
                skill_id: fixture.skill_id.to_string(),
            })
            .unwrap();
        assert_eq!(view.operation_id, "");
        assert_eq!(view.blockers[0].code, "active_deployments");
        assert_eq!(tree_bytes(&fixture.working_bundle), before);
        assert!(
            fs::read_dir(fixture.vault.paths.manager().join("operations"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn move_is_exact_stable_durable_and_replay_is_read_only() {
        let fixture = fixture();
        let expected = tree_bytes(&fixture.working_bundle);
        let (plan, first) = move_to_trash(&fixture);
        assert!(!first.replayed);
        let entry = fixture.vault.paths.trash_entry(
            fixture.skill_id,
            TrashEntryId::from_str(&plan.entry.entry_id).unwrap(),
        );
        assert_eq!(tree_bytes(&entry.join("working/trash-fixture")), expected);
        assert!(!fixture.working_container.exists());
        let manifest = read_trash_entry(&fixture.vault.paths.trash_entry_manifest(
            fixture.skill_id,
            TrashEntryId::from_str(&plan.entry.entry_id).unwrap(),
        ))
        .unwrap();
        assert_eq!(manifest.skill_id, fixture.skill_id);
        assert_eq!(manifest.source_provenance[0].path, fixture.source);
        assert_eq!(
            fixture
                .vault
                .repositories
                .skill(fixture.skill_id)
                .unwrap()
                .unwrap()
                .lifecycle,
            SkillLifecycle::Trashed
        );
        let operation_id = OperationId::from_str(&plan.operation_id).unwrap();
        let counts = fixture.vault.database.execute(move |c| c.query_row(
            "SELECT (SELECT count(*) FROM activity WHERE operation_id=?1), (SELECT count(*) FROM snapshots WHERE operation_id=?1 AND protected=1)",
            [operation_id.to_string()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(crate::persistence::DbExecutorError::Sqlite)).unwrap();
        assert_eq!(counts, (1, 1));
        let before_replay = tree_bytes(fixture.vault.paths.root());
        let replay = fixture
            .service
            .execute_move_to_trash(&TrashExecuteRequest {
                operation_id: plan.operation_id,
                plan_digest: plan.plan_digest,
            })
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(tree_bytes(fixture.vault.paths.root()), before_replay);
    }

    #[test]
    fn restore_preserves_identity_and_never_recreates_deployments() {
        let fixture = fixture();
        let expected = tree_bytes(&fixture.working_bundle);
        let (trash, _) = move_to_trash(&fixture);
        fs::create_dir_all(&fixture.working_container).unwrap();
        fs::write(
            fixture.working_container.join("occupant.txt"),
            b"do not touch",
        )
        .unwrap();
        let restore = fixture
            .service
            .plan_restore(&TrashEntryRequest {
                entry_id: trash.entry.entry_id,
            })
            .unwrap();
        fixture
            .service
            .execute_restore(&TrashExecuteRequest {
                operation_id: restore.operation_id,
                plan_digest: restore.plan_digest,
            })
            .unwrap();
        assert_eq!(
            fs::read(fixture.working_container.join("occupant.txt")).unwrap(),
            b"do not touch"
        );
        let skill = fixture
            .vault
            .repositories
            .skill(fixture.skill_id)
            .unwrap()
            .unwrap();
        assert_eq!(skill.id, fixture.skill_id);
        assert_eq!(skill.lifecycle, SkillLifecycle::Active);
        assert_ne!(
            skill.working_path.as_str().split('/').nth(1).unwrap(),
            fixture.skill_id.to_string()
        );
        assert_eq!(
            tree_bytes(&fixture.vault.paths.root().join(skill.working_path.as_str())),
            expected
        );
        assert!(
            fixture
                .vault
                .repositories
                .skill_deployments(fixture.skill_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn permanent_delete_is_guarded_exact_and_retains_objects_and_history() {
        let fixture = fixture();
        assert!(matches!(
            fixture
                .service
                .plan_permanent_delete(&PermanentDeleteRequest {
                    entry_id: TrashEntryId::generate().to_string(),
                    confirmation: "Trash Fixture".into()
                }),
            Err(TrashError::Missing)
        ));
        let (trash, _) = move_to_trash(&fixture);
        assert!(
            fixture
                .service
                .plan_permanent_delete(&PermanentDeleteRequest {
                    entry_id: trash.entry.entry_id.clone(),
                    confirmation: "wrong".into()
                })
                .is_err()
        );
        let blocked = fixture
            .service
            .plan_permanent_delete(&PermanentDeleteRequest {
                entry_id: trash.entry.entry_id.clone(),
                confirmation: "Trash Fixture".into(),
            })
            .unwrap();
        assert_eq!(blocked.operation_id, "");
        assert_eq!(
            blocked.blockers[0].code,
            "protected_or_unresolved_references"
        );
        fixture
            .vault
            .database
            .execute_critical(|c| {
                c.execute("UPDATE snapshots SET protected=0", [])
                    .map(|_| ())
                    .map_err(crate::persistence::DbExecutorError::Sqlite)
            })
            .unwrap();
        let sibling = fixture.vault.paths.root().join(".manager/trash/leave-me");
        fs::create_dir_all(&sibling).unwrap();
        fs::write(sibling.join("evidence"), b"safe").unwrap();
        let plan = fixture
            .service
            .plan_permanent_delete(&PermanentDeleteRequest {
                entry_id: trash.entry.entry_id,
                confirmation: "Trash Fixture".into(),
            })
            .unwrap();
        fixture
            .service
            .execute_permanent_delete(&TrashExecuteRequest {
                operation_id: plan.operation_id,
                plan_digest: plan.plan_digest,
            })
            .unwrap();
        assert!(sibling.join("evidence").is_file());
        assert!(
            fixture
                .vault
                .objects
                .object_path(
                    fixture
                        .vault
                        .repositories
                        .skill(fixture.skill_id)
                        .unwrap()
                        .unwrap()
                        .working_digest
                )
                .is_dir()
        );
        assert_eq!(
            fixture
                .vault
                .repositories
                .skill(fixture.skill_id)
                .unwrap()
                .unwrap()
                .lifecycle,
            SkillLifecycle::PermanentlyRemoved
        );
        let totals = fixture
            .vault
            .database
            .execute(|c| {
                c.query_row(
                    "SELECT (SELECT count(*) FROM operations), (SELECT count(*) FROM activity)",
                    [],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )
                .map_err(crate::persistence::DbExecutorError::Sqlite)
            })
            .unwrap();
        assert_eq!(totals, (2, 2));
    }

    #[test]
    fn retention_default_is_not_automatic_before_deadline_and_is_after() {
        let now = UtcTimestamp::now();
        let deadline = now.checked_add(DurationMillis(1_000)).unwrap();
        assert!(!automatic_cleanup_eligible(
            TrashPolicy::Retain30Days,
            Some(deadline),
            now
        ));
        assert!(automatic_cleanup_eligible(
            TrashPolicy::Retain30Days,
            Some(deadline),
            deadline
        ));
    }

    #[test]
    fn retention_never_is_never_automatic() {
        let now = UtcTimestamp::now();
        assert!(!automatic_cleanup_eligible(
            TrashPolicy::Never,
            Some(now),
            now
        ));
        assert!(!automatic_cleanup_eligible(TrashPolicy::Never, None, now));
    }

    #[test]
    fn permanent_delete_confirmation_contract_is_exact() {
        let expected = "Example Skill";
        assert_ne!(expected, "example skill");
        assert_ne!(expected, "Example Skill ");
        assert_eq!(expected, "Example Skill");
    }
}
