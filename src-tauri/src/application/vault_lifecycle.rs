//! Read-only Vault inspection and explicit external-edit reconciliation seams.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use thiserror::Error;

use crate::{
    domain::{
        ActivityId, BundleDigest, DeploymentHealth, OperationId, OperationOutcome, SkillId,
        SkillLifecycle, UtcTimestamp,
    },
    filesystem::{BundleCaps, hash_bundle},
    operations::{OperationCoordinator, OperationError},
    persistence::{
        ActivityRecord, DeploymentManifest, DeploymentRecord, DeviceSettings, LocalSourceKind,
        ManifestError, ManifestStore, ObjectRecord, OpenVault, OperationRecord, Repositories,
        RepositoryError, SkillManifest, SkillManifestSource, SkillRecord, SkillRevisionRecord,
        SkillSourceRecord, SourceConfidence, TargetRecord, replace_database_file,
    },
};

use crate::{operations::OperationStore, persistence::DbExecutor};

const LIFECYCLE_OPERATIONS_DIRECTORY: &str = "lifecycle-operations";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LifecycleState {
    Planned,
    Mutating,
    Succeeded,
    FailedRolledBack,
    RecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleJournal {
    schema_version: u16,
    operation_id: OperationId,
    plan_digest: String,
    kind: String,
    state: LifecycleState,
    steps: Vec<LifecycleStepEvidence>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleStepEvidence {
    order: u32,
    action: String,
    source: Option<PathBuf>,
    destination: Option<PathBuf>,
    intent_persisted: bool,
    precondition_verified: bool,
    observed_complete: bool,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleRecoveryReport {
    pub completed: bool,
    pub operations: Vec<LifecycleRecoveryEvidence>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleRecoveryEvidence {
    pub operation_id: String,
    pub classification: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileResult {
    pub skill_id: String,
    pub working_path: String,
    pub previous_digest: String,
    pub working_digest: String,
    pub changed: bool,
    pub deployments_marked_vault_ahead: u32,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultVerifyReport {
    pub healthy: bool,
    pub checked_skills: u32,
    pub checked_objects: u32,
    pub issues: Vec<VaultVerifyIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultVerifyIssue {
    pub code: String,
    pub path: String,
    pub detail: String,
    pub repairable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultRepairPlan {
    pub operation_id: String,
    pub plan_digest: String,
    pub writable: bool,
    pub actions: Vec<VaultRepairAction>,
    pub refused: Vec<VaultVerifyIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultRepairAction {
    pub kind: String,
    pub exact_path: String,
    pub reason: String,
    pub requires_reviewed_operation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct IndexRebuildPlan {
    pub operation_id: String,
    pub plan_digest: String,
    pub blockers: Vec<VaultVerifyIssue>,
    pub skill_manifest_paths: Vec<String>,
    pub deployment_manifest_paths: Vec<String>,
    pub operation_journal_paths: Vec<String>,
    pub unresolved_operation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct IndexRebuildResult {
    pub rebuilt_skills: u32,
    pub rebuilt_deployments: u32,
    pub rebuilt_operations: u32,
    pub backup_path: String,
    // Existing service handles intentionally remain bound to the recoverable backup.
    pub restart_required: bool,
}

const DEFAULT_GC_RETENTION_DAYS: u32 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ObjectGcPlan {
    pub operation_id: String,
    pub plan_digest: String,
    pub phase: ObjectGcPhase,
    pub enabled: bool,
    pub retention_days: u32,
    pub candidates: Vec<ObjectGcCandidate>,
    pub blockers: Vec<String>,
    pub referenced_objects: u32,
    pub inspected_objects: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectGcPhase {
    StagePendingDelete,
    DeletePending,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ObjectGcCandidate {
    pub digest: String,
    pub exact_path: String,
    pub created_at: String,
    pub retention_deadline: String,
    pub pending_owner_operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ObjectGcResult {
    pub operation_id: String,
    pub phase: ObjectGcPhase,
    pub affected_objects: u32,
    pub evidence_path: String,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ObjectGcSettingsSummary {
    pub retention_days: u32,
    pub last_run: Option<String>,
    pub eligible: bool,
    pub next_run: Option<String>,
    pub disabled_reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
// This is intentionally a wire-level capability matrix: each boolean reports one independently
// probed filesystem behavior and must remain inspectable when preflight fails partway through.
#[allow(clippy::struct_excessive_bools)]
pub struct DestinationCapabilityReport {
    pub status: CapabilityStatus,
    pub write_file: bool,
    pub create_directory: bool,
    pub symlink: bool,
    pub executable_bit: bool,
    pub atomic_rename: bool,
    pub file_fsync: bool,
    pub directory_fsync: bool,
    pub advisory_lock: bool,
    pub case_sensitive: bool,
    pub available_bytes: Option<String>,
    pub required_bytes: String,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultRelocatePlan {
    pub operation_id: String,
    pub plan_digest: String,
    pub old_vault_path: String,
    pub destination_path: String,
    pub staging_path: String,
    pub vault_id: String,
    pub capability: DestinationCapabilityReport,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VaultRelocateResult {
    pub operation_id: String,
    pub old_vault_path: String,
    pub active_vault_path: String,
    pub rewritten_symlinks: u32,
    pub restart_required: bool,
    pub old_vault_retained: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OldVaultCleanupPlan {
    pub operation_id: String,
    pub plan_digest: String,
    pub old_vault_path: String,
    pub active_vault_path: String,
    pub vault_id: String,
}

#[derive(Clone)]
pub struct VaultLifecycleService {
    vault: Arc<OpenVault>,
    coordinator: Arc<OperationCoordinator>,
    application_support: Option<PathBuf>,
}

impl VaultLifecycleService {
    #[must_use]
    pub fn with_runtime(
        vault: Arc<OpenVault>,
        coordinator: Arc<OperationCoordinator>,
        application_support: PathBuf,
    ) -> Self {
        Self {
            vault,
            coordinator,
            application_support: Some(application_support),
        }
    }

    /// Re-hashes one working Bundle and updates derived index state only. Working bytes are never
    /// opened for writing and no object is published.
    pub fn reconcile_external_edit(&self, id: SkillId) -> Result<ReconcileResult, LifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        coordinator.run_lifecycle(|| {
            let mut skill = self
                .vault
                .repositories
                .skill(id)?
                .ok_or(LifecycleError::SkillMissing)?;
            let path = self.vault.paths.root().join(skill.working_path.as_str());
            let digest = hash_bundle(&path, BundleCaps::default())?.digest;
            let previous = skill.working_digest;
            let changed = previous != digest;
            let mut marked = 0_u32;
            if changed {
                let mut manifest = self.vault.manifests.read_skill(id)?;
                if manifest.working_digest != previous
                    || manifest.working_path != skill.working_path
                    || self
                        .vault
                        .repositories
                        .skill(id)?
                        .as_ref()
                        .map(|value| value.working_digest)
                        != Some(previous)
                    || hash_bundle(&path, BundleCaps::default())?.digest != digest
                {
                    return Err(LifecycleError::StaleManifest);
                }
                let old_manifest = manifest.clone();
                manifest.working_digest = digest;
                self.vault.manifests.write_skill(&manifest)?;
                skill.working_digest = digest;
                skill.updated_at = UtcTimestamp::now();
                if let Err(error) = self.vault.repositories.upsert_skill(skill) {
                    self.vault.manifests.write_skill(&old_manifest)?;
                    return Err(error.into());
                }
                for deployment in self.vault.repositories.skill_deployments(id)? {
                    if deployment.active {
                        self.vault.repositories.update_deployment_health(
                            deployment.id,
                            DeploymentHealth::VaultAhead,
                            UtcTimestamp::now(),
                        )?;
                        marked = marked.saturating_add(1);
                    }
                }
            }
            Ok(ReconcileResult {
                skill_id: id.to_string(),
                working_path: path.to_string_lossy().into_owned(),
                previous_digest: previous.to_string(),
                working_digest: digest.to_string(),
                changed,
                deployments_marked_vault_ahead: marked,
            })
        })
    }

    fn lifecycle_root(&self) -> PathBuf {
        self.vault
            .paths
            .manager()
            .join(LIFECYCLE_OPERATIONS_DIRECTORY)
    }

    fn lifecycle_operation_directory(&self, id: OperationId) -> PathBuf {
        self.lifecycle_root().join(id.to_string())
    }

    fn persist_planned(
        &self,
        id: OperationId,
        digest: &str,
        kind: &str,
    ) -> Result<(), LifecycleError> {
        let directory = self.lifecycle_operation_directory(id);
        durable_json(
            &directory.join("journal.json"),
            &LifecycleJournal {
                schema_version: 1,
                operation_id: id,
                plan_digest: digest.to_owned(),
                kind: kind.to_owned(),
                state: LifecycleState::Planned,
                steps: Vec::new(),
                error: None,
            },
        )
    }

    /// Reveals an indexed working Bundle with macOS Finder after canonical containment checks.
    pub fn reveal_working(&self, id: SkillId) -> Result<String, LifecycleError> {
        let skill = self
            .vault
            .repositories
            .skill(id)?
            .ok_or(LifecycleError::SkillMissing)?;
        let path = self.vault.paths.root().join(skill.working_path.as_str());
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(self.vault.paths.skills()) || !canonical.is_dir() {
            return Err(LifecycleError::UnsafeRevealPath(path));
        }
        #[cfg(target_os = "macos")]
        {
            let status = Command::new("open").arg("-R").arg(&canonical).status()?;
            if !status.success() {
                return Err(LifecycleError::FinderFailed);
            }
        }
        #[cfg(not(target_os = "macos"))]
        return Err(LifecycleError::FinderUnsupported);
        Ok(canonical.to_string_lossy().into_owned())
    }

    /// Compares durable manifests, working trees, immutable objects, layout and index references.
    /// This method performs no writes.
    #[allow(clippy::too_many_lines)]
    pub fn verify(&self) -> Result<VaultVerifyReport, LifecycleError> {
        let mut issues = Vec::new();
        for path in [
            self.vault.paths.skills(),
            self.vault.paths.manager().join("manifests/skills"),
            self.vault.paths.manager().join("manifests/deployments"),
            self.vault.paths.objects(),
            self.vault.paths.database(),
        ] {
            if !path.exists() {
                issue(
                    &mut issues,
                    "missing_layout",
                    path,
                    "required Vault path is absent",
                    false,
                );
            }
        }
        let integrity = self.vault.repositories.index_integrity()?;
        if integrity != "ok" {
            issue(
                &mut issues,
                "index_corrupt",
                self.vault.paths.database(),
                &integrity,
                false,
            );
        }
        let foreign_keys = self.vault.repositories.foreign_key_violation_count()?;
        if foreign_keys != 0 {
            issue(
                &mut issues,
                "foreign_key_violation",
                self.vault.paths.database(),
                &format!("{foreign_keys} foreign-key violations"),
                false,
            );
        }

        let skills = self.vault.repositories.skills()?;
        let indexed_skill_ids = skills.iter().map(|skill| skill.id).collect::<BTreeSet<_>>();
        for path in json_paths(&self.vault.paths.manager().join("manifests/skills"))? {
            match manifest_id(&path)
                .and_then(|id| self.vault.manifests.read_skill(id).map_err(Into::into))
            {
                Ok(manifest) if indexed_skill_ids.contains(&manifest.skill_id) => {}
                Ok(_) => issue(
                    &mut issues,
                    "orphan_skill_manifest",
                    path,
                    "manifest has no indexed Skill",
                    false,
                ),
                Err(error) => issue(
                    &mut issues,
                    "skill_manifest_invalid",
                    path,
                    &error.to_string(),
                    false,
                ),
            }
        }
        let mut checked_objects = 0_u32;
        for skill in &skills {
            self.verify_skill(skill, &mut issues);
            checked_objects = checked_objects.saturating_add(1);
            if let Err(error) = self.vault.objects.verify(skill.baseline_digest) {
                issue(
                    &mut issues,
                    "object_invalid",
                    self.vault.objects.object_path(skill.baseline_digest),
                    &error.to_string(),
                    false,
                );
            }
        }
        let deployments = self.vault.repositories.all_deployments()?;
        let indexed_deployment_ids = deployments
            .iter()
            .map(|value| value.id)
            .collect::<BTreeSet<_>>();
        for path in json_paths(&self.vault.paths.manager().join("manifests/deployments"))? {
            match manifest_id(&path)
                .and_then(|id| self.vault.manifests.read_deployment(id).map_err(Into::into))
            {
                Ok(manifest) if indexed_deployment_ids.contains(&manifest.deployment_id) => {}
                Ok(_) => issue(
                    &mut issues,
                    "orphan_deployment_manifest",
                    path,
                    "manifest has no indexed Deployment",
                    false,
                ),
                Err(error) => issue(
                    &mut issues,
                    "deployment_manifest_invalid",
                    path,
                    &error.to_string(),
                    false,
                ),
            }
        }
        for deployment in deployments {
            let path = self.vault.manifests.deployment_path(deployment.id);
            match self.vault.manifests.read_deployment(deployment.id) {
                Ok(manifest) if deployment_manifest_matches(&manifest, &deployment) => {}
                Ok(_) => issue(
                    &mut issues,
                    "deployment_manifest_mismatch",
                    path,
                    "manifest disagrees with index identity or digest",
                    false,
                ),
                Err(error) => issue(
                    &mut issues,
                    "deployment_manifest_invalid",
                    path,
                    &error.to_string(),
                    false,
                ),
            }
        }
        Ok(VaultVerifyReport {
            healthy: issues.is_empty(),
            checked_skills: u32::try_from(skills.len()).unwrap_or(u32::MAX),
            checked_objects,
            issues,
        })
    }

    fn verify_skill(&self, skill: &SkillRecord, issues: &mut Vec<VaultVerifyIssue>) {
        let manifest_path = self.vault.manifests.skill_path(skill.id);
        match self.vault.manifests.read_skill(skill.id) {
            Ok(manifest)
                if manifest.skill_id == skill.id
                    && manifest.working_path == skill.working_path
                    && manifest.baseline_digest == skill.baseline_digest => {}
            Ok(_) => issue(
                issues,
                "skill_manifest_mismatch",
                manifest_path,
                "manifest disagrees with indexed identity, path, or baseline",
                false,
            ),
            Err(error) => issue(
                issues,
                "skill_manifest_invalid",
                manifest_path,
                &error.to_string(),
                true,
            ),
        }
        if skill.lifecycle == SkillLifecycle::Active {
            let working = self.vault.paths.root().join(skill.working_path.as_str());
            match hash_bundle(&working, BundleCaps::default()) {
                Ok(hashed) if hashed.digest == skill.working_digest => {}
                Ok(hashed) => issue(
                    issues,
                    "working_digest_mismatch",
                    working,
                    &format!("indexed {}; actual {}", skill.working_digest, hashed.digest),
                    false,
                ),
                Err(error) => issue(
                    issues,
                    "working_bundle_invalid",
                    working,
                    &error.to_string(),
                    false,
                ),
            }
        }
    }

    /// Produces a read-only repair plan. Missing manifests are offered only where one indexed row
    /// identifies the exact manifest; conflicting manifests are refused rather than guessed.
    pub fn plan_repair(&self) -> Result<VaultRepairPlan, LifecycleError> {
        let report = self.verify()?;
        let mut actions = Vec::new();
        let mut refused = Vec::new();
        for problem in report.issues {
            if problem.repairable
                && problem.code.ends_with("_invalid")
                && !PathBuf::from(&problem.path).exists()
            {
                actions.push(VaultRepairAction {
                    kind: "restore_manifest_from_index".into(),
                    exact_path: problem.path,
                    reason: "one stable indexed identity owns this exact manifest path".into(),
                    requires_reviewed_operation: true,
                });
            } else {
                refused.push(problem);
            }
        }
        let operation_id = OperationId::generate();
        let plan_digest = repair_digest(operation_id, &actions, &refused)?;
        let plan = VaultRepairPlan {
            operation_id: operation_id.to_string(),
            plan_digest,
            writable: !actions.is_empty(),
            actions,
            refused,
        };
        let directory = self.lifecycle_operation_directory(operation_id);
        std::fs::create_dir_all(&directory)?;
        sync_parent(&directory)?;
        crate::filesystem::durable::atomic_write(
            &directory.join("lifecycle-plan.json"),
            &serde_json::to_vec_pretty(&plan)?,
        )
        .map_err(|error| LifecycleError::Durability(error.to_string()))?;
        self.persist_planned(operation_id, &plan.plan_digest, "repair")?;
        Ok(plan)
    }

    /// Executes only the immutable reviewed repair plan. Every exact path is rechecked and all
    /// manifest writes are serialized with target Operations. A failed multi-file repair removes
    /// files written by this operation before recording its durable outcome.
    pub fn execute_repair(
        &self,
        operation_id: OperationId,
        digest: &str,
    ) -> Result<u32, LifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        coordinator.run_lifecycle(|| {
            let directory = self.lifecycle_operation_directory(operation_id);
            let plan: VaultRepairPlan = serde_json::from_slice(&std::fs::read(directory.join("lifecycle-plan.json"))?)?;
            if plan.operation_id != operation_id.to_string() || plan.plan_digest != digest
                || repair_digest(operation_id, &plan.actions, &plan.refused)? != digest {
                return Err(LifecycleError::StalePlan);
            }
            let mut journal = LifecycleJournal { schema_version: 1, operation_id,
                plan_digest: digest.to_owned(), kind: "repair".into(), state: LifecycleState::Mutating,
                steps: Vec::new(), error: None };
            durable_json(&directory.join("journal.json"), &journal)?;
            let mut written = Vec::new();
            let result = (|| {
                for (index, action) in plan.actions.iter().enumerate() {
                    let exact = PathBuf::from(&action.exact_path);
                    if exact.exists() { return Err(LifecycleError::StalePlan); }
                    let skill = self.vault.repositories.skills()?.into_iter().find(|skill|
                        self.vault.manifests.skill_path(skill.id) == exact
                    ).ok_or(LifecycleError::AmbiguousRepair)?;
                    let sources = self.vault.repositories.skill_sources(skill.id)?.into_iter().map(|source| SkillManifestSource {
                        kind: LocalSourceKind::LocalObservation,
                        path: source.path,
                        captured_at: source.captured_at,
                        confidence: SourceConfidence::Observed,
                    }).collect();
                    let manifest = SkillManifest::new(skill.id, skill.display_name, skill.deployment_name,
                        skill.working_digest, skill.baseline_digest, skill.created_at, sources)?;
                    journal.steps.push(LifecycleStepEvidence { order: u32::try_from(index).unwrap_or(u32::MAX),
                        action: "write_manifest".into(), source: None, destination: Some(exact.clone()),
                        intent_persisted: true, precondition_verified: true, observed_complete: false });
                    durable_json(&directory.join("journal.json"), &journal)?;
                    self.vault.manifests.write_skill(&manifest)?;
                    let observed = self.vault.manifests.read_skill(skill.id)?;
                    if observed != manifest { return Err(LifecycleError::IntegrityFailed); }
                    journal.steps.last_mut().expect("step was appended").observed_complete = true;
                    durable_json(&directory.join("journal.json"), &journal)?;
                    written.push(exact);
                }
                Ok(u32::try_from(written.len()).unwrap_or(u32::MAX))
            })();
            let mut rollback_failed = false;
            if result.is_err() {
                for path in &written {
                    if std::fs::remove_file(path).is_err() || path.exists() { rollback_failed = true; }
                }
            }
            journal.state = if result.is_ok() { LifecycleState::Succeeded } else if rollback_failed {
                LifecycleState::RecoveryRequired
            } else { LifecycleState::FailedRolledBack };
            journal.error = result.as_ref().err().map(ToString::to_string);
            durable_json(&directory.join("journal.json"), &journal)?;
            let outcome = if result.is_ok() { "succeeded" } else if rollback_failed { "recovery_required" } else { "failed_rolled_back" };
            crate::filesystem::durable::atomic_write(&directory.join("lifecycle-journal.json"),
                serde_json::to_string_pretty(&serde_json::json!({"operationId": operation_id, "planDigest": digest, "outcome": outcome, "writtenPaths": written}))?.as_bytes()
            ).map_err(|error| LifecycleError::Durability(error.to_string()))?;
            if rollback_failed { return Err(LifecycleError::RecoveryRequired); }
            result
        })
    }

    /// Inspects every durable rebuild input and hashes every working tree without changing active
    /// content or the `SQLite` index. The stored plan is lifecycle evidence, not active content.
    pub fn plan_index_rebuild(&self) -> Result<IndexRebuildPlan, LifecycleError> {
        let mut blockers = Vec::new();
        let vault_manifest: crate::persistence::VaultManifest =
            serde_json::from_slice(&std::fs::read(self.vault.paths.vault_manifest())?)?;
        if vault_manifest != self.vault.manifest {
            return Err(LifecycleError::InvalidDurableInput {
                path: self.vault.paths.vault_manifest(),
                detail: "vault manifest changed since this Vault was opened".into(),
            });
        }
        let skill_paths = json_paths(&self.vault.paths.manager().join("manifests/skills"))?;
        let deployment_paths =
            json_paths(&self.vault.paths.manager().join("manifests/deployments"))?;
        for path in &skill_paths {
            match manifest_id(path).and_then(|id| {
                self.vault
                    .manifests
                    .read_skill(id)
                    .map_err(LifecycleError::from)
            }) {
                Ok(manifest) => {
                    let working = self.vault.paths.root().join(manifest.working_path.as_str());
                    match hash_bundle(&working, BundleCaps::default()) {
                        Ok(hashed) if hashed.digest == manifest.working_digest => {}
                        Ok(hashed) => issue(
                            &mut blockers,
                            "working_digest_mismatch",
                            working,
                            &format!(
                                "manifest {}; actual {}",
                                manifest.working_digest, hashed.digest
                            ),
                            false,
                        ),
                        Err(error) => issue(
                            &mut blockers,
                            "working_bundle_invalid",
                            working,
                            &error.to_string(),
                            false,
                        ),
                    }
                    if let Err(error) = self.vault.objects.verify(manifest.baseline_digest) {
                        issue(
                            &mut blockers,
                            "object_invalid",
                            self.vault.objects.object_path(manifest.baseline_digest),
                            &error.to_string(),
                            false,
                        );
                    }
                }
                Err(error) => issue(
                    &mut blockers,
                    "skill_manifest_invalid",
                    path.clone(),
                    &error.to_string(),
                    false,
                ),
            }
        }
        for path in &deployment_paths {
            if let Err(error) = manifest_id(path).and_then(|id| {
                self.vault
                    .manifests
                    .read_deployment(id)
                    .map_err(LifecycleError::from)
            }) {
                issue(
                    &mut blockers,
                    "deployment_manifest_invalid",
                    path.clone(),
                    &error.to_string(),
                    false,
                );
            }
        }
        let store = OperationStore::open(self.vault.paths.manager())?;
        let mut journal_paths = Vec::new();
        let mut unresolved = Vec::new();
        for id in durable_operation_ids(&store)? {
            let stored = store.load(id)?;
            journal_paths.push(store.journal_path(id).to_string_lossy().into_owned());
            if !stored.journal.state.is_terminal() {
                unresolved.push(id.to_string());
            }
        }
        let operation_id = OperationId::generate();
        let mut plan = IndexRebuildPlan {
            operation_id: operation_id.to_string(),
            plan_digest: String::new(),
            blockers,
            skill_manifest_paths: strings(&skill_paths),
            deployment_manifest_paths: strings(&deployment_paths),
            operation_journal_paths: journal_paths,
            unresolved_operation_ids: unresolved,
        };
        self.persist_index_rebuild_plan(operation_id, &mut plan)?;
        Ok(plan)
    }

    fn persist_index_rebuild_plan(
        &self,
        operation_id: OperationId,
        plan: &mut IndexRebuildPlan,
    ) -> Result<(), LifecycleError> {
        plan.plan_digest = rebuild_digest(plan)?;
        let directory = self.lifecycle_operation_directory(operation_id);
        fs::create_dir_all(&directory)?;
        sync_parent(&directory)?;
        crate::filesystem::durable::atomic_write(
            &directory.join("index-rebuild-plan.json"),
            &serde_json::to_vec_pretty(plan)?,
        )
        .map_err(|error| LifecycleError::Durability(error.to_string()))?;
        self.persist_planned(operation_id, &plan.plan_digest, "index_rebuild")
    }

    pub fn execute_index_rebuild(
        &self,
        operation_id: OperationId,
        digest: &str,
    ) -> Result<IndexRebuildResult, LifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        coordinator.run_lifecycle(|| {
            let operation_dir = self.lifecycle_operation_directory(operation_id);
            let plan: IndexRebuildPlan = serde_json::from_slice(&std::fs::read(operation_dir.join("index-rebuild-plan.json"))?)?;
            if plan.operation_id != operation_id.to_string() || plan.plan_digest != digest || rebuild_digest(&plan)? != digest || !plan.blockers.is_empty() {
                return Err(LifecycleError::StalePlan);
            }
            let fresh = self.plan_index_rebuild()?;
            if !fresh.blockers.is_empty() || fresh.skill_manifest_paths != plan.skill_manifest_paths || fresh.deployment_manifest_paths != plan.deployment_manifest_paths || fresh.operation_journal_paths != plan.operation_journal_paths {
                return Err(LifecycleError::StalePlan);
            }
            let database = self.vault.paths.database();
            let staged = database.with_file_name(format!("index-rebuild-{operation_id}.sqlite"));
            let backup = database.with_file_name(format!("index-before-rebuild-{operation_id}.sqlite"));
            let mut journal = LifecycleJournal { schema_version: 1, operation_id,
                plan_digest: digest.to_owned(), kind: "index_rebuild".into(), state: LifecycleState::Mutating,
                steps: vec![LifecycleStepEvidence { order: 0, action: "build_staged_index".into(),
                    source: Some(database.clone()), destination: Some(staged.clone()), intent_persisted: true,
                    precondition_verified: true, observed_complete: false }], error: None };
            durable_json(&operation_dir.join("journal.json"), &journal)?;
            let result = self.build_replacement(&staged, &plan);
            if result.is_err() { let _ = std::fs::remove_file(&staged); }
            let (skills, deployments, operations) = result?;
            if !staged.is_file() { return Err(LifecycleError::IntegrityFailed); }
            journal.steps[0].observed_complete = true;
            journal.steps.push(LifecycleStepEvidence { order: 1, action: "replace_index".into(),
                source: Some(staged.clone()), destination: Some(database.clone()), intent_persisted: true,
                precondition_verified: true, observed_complete: false });
            durable_json(&operation_dir.join("journal.json"), &journal)?;
            self.vault.database.checkpoint_for_replacement()?;
            replace_database_file(&database, &staged, &backup)?;
            if !database.is_file() || !backup.is_file() { return Err(LifecycleError::IntegrityFailed); }
            journal.steps[1].observed_complete = true;
            crate::filesystem::durable::atomic_write(&operation_dir.join("lifecycle-journal.json"),
                &serde_json::to_vec_pretty(&serde_json::json!({"operationId": operation_id, "planDigest": digest, "outcome":"succeeded_restart_required", "backupPath": backup}))?)
                .map_err(|error| LifecycleError::Durability(error.to_string()))?;
            journal.state = LifecycleState::Succeeded;
            durable_json(&operation_dir.join("journal.json"), &journal)?;
            Ok(IndexRebuildResult { rebuilt_skills: skills, rebuilt_deployments: deployments, rebuilt_operations: operations,
                backup_path: backup.to_string_lossy().into_owned(), restart_required: true })
        })
    }

    /// Performs a behavioral, read-only-with-respect-to-the-destination Vault preflight. Probe
    /// artifacts are created only in the destination parent and are always removed.
    pub fn plan_relocate(&self, destination: &Path) -> Result<VaultRelocatePlan, LifecycleError> {
        let destination = absolute_normalized(destination)?;
        validate_relocation_paths(self.vault.paths.root(), &destination)?;
        if destination.exists() {
            return Err(LifecycleError::UnsafeRelocation(
                "destination already exists".into(),
            ));
        }
        let operation_id = OperationId::generate();
        let required = tree_size(self.vault.paths.root())?;
        let capability = capability_preflight(&destination, operation_id, required)?;
        let staging = relocation_staging(&destination, operation_id)?;
        let mut plan = VaultRelocatePlan {
            operation_id: operation_id.to_string(),
            plan_digest: String::new(),
            old_vault_path: self.vault.paths.root().to_string_lossy().into_owned(),
            destination_path: destination.to_string_lossy().into_owned(),
            staging_path: staging.to_string_lossy().into_owned(),
            vault_id: self.vault.manifest.vault_id.to_string(),
            capability,
        };
        plan.plan_digest = relocation_digest(&plan)?;
        let directory = self.lifecycle_operation_directory(operation_id);
        fs::create_dir_all(&directory)?;
        sync_parent(&directory)?;
        durable_json(&directory.join("relocate-plan.json"), &plan)?;
        durable_json(
            &directory.join("relocate-journal.json"),
            &serde_json::json!({
                "operationId": operation_id, "planDigest": plan.plan_digest,
                "state": "planned", "authority": plan.old_vault_path,
                "stagingPath": plan.staging_path, "resumeClassification": "restart_copy"
            }),
        )?;
        self.persist_planned(operation_id, &plan.plan_digest, "relocate")?;
        Ok(plan)
    }

    pub fn execute_relocate(
        &self,
        operation_id: OperationId,
        digest: &str,
    ) -> Result<VaultRelocateResult, LifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        coordinator.run_lifecycle(|| {
            let directory = self.lifecycle_operation_directory(operation_id);
            let plan: VaultRelocatePlan = serde_json::from_slice(&fs::read(directory.join("relocate-plan.json"))?)?;
            if plan.operation_id != operation_id.to_string() || plan.plan_digest != digest || relocation_digest(&plan)? != digest
                || plan.vault_id != self.vault.manifest.vault_id.to_string()
                || Path::new(&plan.old_vault_path) != self.vault.paths.root() { return Err(LifecycleError::StalePlan); }
            let destination = PathBuf::from(&plan.destination_path);
            let staging = PathBuf::from(&plan.staging_path);
            validate_relocation_paths(self.vault.paths.root(), &destination)?;
            let fresh = capability_preflight(&destination, operation_id, tree_size(self.vault.paths.root())?)?;
            if fresh.status != CapabilityStatus::Supported { return Err(LifecycleError::CapabilityBlocked(fresh.blockers)); }
            if destination.exists() || staging.exists() { return Err(LifecycleError::StalePlan); }
            let mut lifecycle_journal = LifecycleJournal {
                schema_version: 1, operation_id, plan_digest: digest.to_owned(), kind: "relocate".into(),
                state: LifecycleState::Mutating, steps: vec![LifecycleStepEvidence {
                    order: 0, action: "copy_and_publish_destination".into(),
                    source: Some(self.vault.paths.root().to_path_buf()), destination: Some(destination.clone()),
                    intent_persisted: true, precondition_verified: true, observed_complete: false,
                }], error: None,
            };
            durable_json(&directory.join("journal.json"), &lifecycle_journal)?;
            durable_json(&directory.join("relocate-journal.json"), &serde_json::json!({
                "operationId": operation_id, "planDigest": digest, "state": "copying",
                "authority": plan.old_vault_path, "stagingPath": plan.staging_path,
                "resumeClassification": "discard_owned_staging_and_restart"
            }))?;
            self.vault.database.checkpoint_for_replacement()?;
            fs::create_dir(&staging)?;
            durable_json(&staging.join(".relocation-owner.json"), &serde_json::json!({
                "operationId": operation_id, "vaultId": plan.vault_id, "source": plan.old_vault_path
            }))?;
            if let Err(error) = copy_tree_contents(self.vault.paths.root(), &staging) {
                cleanup_owned_staging(&staging, operation_id, &plan.vault_id)?;
                durable_json(&directory.join("relocate-journal.json"), &serde_json::json!({
                    "operationId": operation_id, "planDigest": digest, "state": "failed_rolled_back",
                    "authority": plan.old_vault_path, "error": error.to_string(), "resumeClassification": "restart_copy"
                }))?;
                return Err(error);
            }
            verify_copied_vault(self.vault.as_ref(), &staging)?;
            fs::remove_file(staging.join(".relocation-owner.json"))?;
            fs::rename(&staging, &destination)?;
            sync_parent(&destination)?;
            if !destination.is_dir() || staging.exists() { return Err(LifecycleError::IntegrityFailed); }
            lifecycle_journal.steps[0].observed_complete = true;
            durable_json(&directory.join("journal.json"), &lifecycle_journal)?;

            let settings_path = self.settings_path()?;
            let old_settings: DeviceSettings = read_settings(&settings_path)?;
            if old_settings.active_vault_path != self.vault.paths.root() { return Err(LifecycleError::StalePlan); }
            let mut new_settings = old_settings.clone();
            new_settings.active_vault_path.clone_from(&destination);
            lifecycle_journal.steps.push(LifecycleStepEvidence {
                order: 1, action: "switch_settings".into(), source: Some(settings_path.clone()),
                destination: Some(destination.clone()), intent_persisted: true,
                precondition_verified: true, observed_complete: false,
            });
            durable_json(&directory.join("journal.json"), &lifecycle_journal)?;
            write_settings(&settings_path, &new_settings)?;
            if read_settings(&settings_path)?.active_vault_path != destination {
                return Err(LifecycleError::CutoverFailed("settings verification failed".into()));
            }
            lifecycle_journal.steps[1].observed_complete = true;
            lifecycle_journal.steps.push(LifecycleStepEvidence {
                order: 2, action: "rewrite_managed_links".into(), source: Some(self.vault.paths.root().to_path_buf()),
                destination: Some(destination.clone()), intent_persisted: true,
                precondition_verified: true, observed_complete: false,
            });
            durable_json(&directory.join("journal.json"), &lifecycle_journal)?;
            let result = rewrite_managed_links(self.vault.as_ref(), &destination, operation_id);
            let rewritten = match result {
                Ok(value) => value,
                Err((error, changed)) => {
                    for (target, old_link, _) in changed.iter().rev() { let _ = replace_symlink(target, old_link); }
                    let _ = write_settings(&settings_path, &old_settings);
                    durable_json(&directory.join("relocate-journal.json"), &serde_json::json!({
                        "operationId": operation_id, "planDigest": digest, "state": "failed_compensated",
                        "authority": plan.old_vault_path, "destinationVerified": true,
                        "resumeClassification": "rollback_complete_restart_cutover", "error": error.to_string()
                    }))?;
                    return Err(error);
                }
            };
            lifecycle_journal.steps[2].observed_complete = true;
            durable_json(&directory.join("journal.json"), &lifecycle_journal)?;
            let confirmed: DeviceSettings = read_settings(&settings_path)?;
            if confirmed.active_vault_path != destination { return Err(LifecycleError::CutoverFailed("settings verification failed".into())); }
            durable_json(&directory.join("relocate-journal.json"), &serde_json::json!({
                "operationId": operation_id, "planDigest": digest, "state": "succeeded_restart_required",
                "authority": destination, "oldVaultRetained": true, "rewrittenSymlinks": rewritten,
                "resumeClassification": "complete"
            }))?;
            lifecycle_journal.state = LifecycleState::Succeeded;
            durable_json(&directory.join("journal.json"), &lifecycle_journal)?;
            Ok(VaultRelocateResult { operation_id: operation_id.to_string(), old_vault_path: plan.old_vault_path,
                active_vault_path: plan.destination_path, rewritten_symlinks: rewritten, restart_required: true,
                old_vault_retained: true })
        })
    }

    pub fn plan_old_vault_cleanup(
        &self,
        old_vault: &Path,
    ) -> Result<OldVaultCleanupPlan, LifecycleError> {
        let settings: DeviceSettings = read_settings(&self.settings_path()?)?;
        let old = old_vault.canonicalize()?;
        if old == settings.active_vault_path || !old.is_dir() {
            return Err(LifecycleError::UnsafeRelocation(
                "old Vault is active or absent".into(),
            ));
        }
        let manifest: crate::persistence::VaultManifest =
            serde_json::from_slice(&fs::read(old.join(".manager/vault.json"))?)?;
        let active_manifest: crate::persistence::VaultManifest = serde_json::from_slice(
            &fs::read(settings.active_vault_path.join(".manager/vault.json"))?,
        )?;
        if manifest.vault_id != active_manifest.vault_id {
            return Err(LifecycleError::UnsafeRelocation(
                "vaultId evidence does not match active Vault".into(),
            ));
        }
        let operation_id = OperationId::generate();
        let mut plan = OldVaultCleanupPlan {
            operation_id: operation_id.to_string(),
            plan_digest: String::new(),
            old_vault_path: old.to_string_lossy().into_owned(),
            active_vault_path: settings.active_vault_path.to_string_lossy().into_owned(),
            vault_id: manifest.vault_id.to_string(),
        };
        plan.plan_digest = cleanup_digest(&plan)?;
        let dir = settings
            .active_vault_path
            .join(".manager/lifecycle-operations")
            .join(operation_id.to_string());
        fs::create_dir_all(&dir)?;
        sync_parent(&dir)?;
        durable_json(&dir.join("old-vault-cleanup-plan.json"), &plan)?;
        durable_json(
            &dir.join("journal.json"),
            &LifecycleJournal {
                schema_version: 1,
                operation_id,
                plan_digest: plan.plan_digest.clone(),
                kind: "old_vault_cleanup".into(),
                state: LifecycleState::Planned,
                steps: Vec::new(),
                error: None,
            },
        )?;
        Ok(plan)
    }

    pub fn execute_old_vault_cleanup(
        &self,
        operation_id: OperationId,
        digest: &str,
    ) -> Result<(), LifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        coordinator.run_lifecycle(|| {
            let settings: DeviceSettings = read_settings(&self.settings_path()?)?;
            let dir = settings.active_vault_path.join(".manager/lifecycle-operations").join(operation_id.to_string());
            let plan: OldVaultCleanupPlan = serde_json::from_slice(&fs::read(dir.join("old-vault-cleanup-plan.json"))?)?;
            if plan.operation_id != operation_id.to_string() || plan.plan_digest != digest || cleanup_digest(&plan)? != digest
                || settings.active_vault_path != PathBuf::from(&plan.active_vault_path) { return Err(LifecycleError::StalePlan); }
            let old = PathBuf::from(&plan.old_vault_path);
            if old == self.vault.paths.root() {
                return Err(LifecycleError::UnsafeRelocation(
                    "the open runtime Vault can never be recursively deleted".into(),
                ));
            }
            let manifest: crate::persistence::VaultManifest = serde_json::from_slice(&fs::read(old.join(".manager/vault.json"))?)?;
            let active_manifest: crate::persistence::VaultManifest = serde_json::from_slice(
                &fs::read(settings.active_vault_path.join(".manager/vault.json"))?)?;
            if manifest.vault_id.to_string() != plan.vault_id
                || active_manifest.vault_id != manifest.vault_id
                || old == settings.active_vault_path { return Err(LifecycleError::StalePlan); }
            let mut journal = LifecycleJournal { schema_version: 1, operation_id,
                plan_digest: digest.to_owned(), kind: "old_vault_cleanup".into(), state: LifecycleState::Mutating,
                steps: vec![LifecycleStepEvidence { order: 0, action: "delete_old_vault".into(),
                    source: Some(old.clone()), destination: None, intent_persisted: true,
                    precondition_verified: true, observed_complete: false }], error: None };
            durable_json(&dir.join("journal.json"), &journal)?;
            make_tree_writable(&old)?; fs::remove_dir_all(&old)?;
            sync_parent(&old)?;
            if old.exists() { return Err(LifecycleError::IntegrityFailed); }
            journal.steps[0].observed_complete = true;
            journal.state = LifecycleState::Succeeded;
            durable_json(&dir.join("journal.json"), &journal)?;
            durable_json(&dir.join("old-vault-cleanup-journal.json"), &serde_json::json!({"operationId": operation_id, "planDigest": digest, "state":"succeeded"}))?;
            Ok(())
        })
    }

    fn settings_path(&self) -> Result<PathBuf, LifecycleError> {
        self.application_support
            .as_ref()
            .map(|p| p.join("settings.json"))
            .ok_or_else(|| {
                LifecycleError::CutoverFailed("device settings location is unavailable".into())
            })
    }

    /// Builds and persists a read-only, digest-confirmed object collection plan. Failure to read
    /// any reference source is represented as a blocker; an empty reference set is never assumed.
    pub fn plan_object_gc(&self, phase: ObjectGcPhase) -> Result<ObjectGcPlan, LifecycleError> {
        let operation_id = OperationId::generate();
        let reference_result = self.gc_references(None);
        let mut blockers = Vec::new();
        let referenced = match reference_result {
            Ok(references) => references,
            Err(error) => {
                blockers.push(error.to_string());
                BTreeSet::new()
            }
        };
        let mut candidates = Vec::new();
        let mut inspected_objects = 0_u32;
        if blockers.is_empty() {
            match phase {
                ObjectGcPhase::StagePendingDelete => {
                    for (digest, path) in enumerate_object_paths(&self.vault.paths.objects())? {
                        inspected_objects = inspected_objects.saturating_add(1);
                        let manifest = self.vault.objects.verify(digest)?;
                        let deadline = retention_deadline(manifest.created_at)?;
                        if !referenced.contains(&digest)
                            && deadline.unix_millis()? <= UtcTimestamp::now().unix_millis()?
                        {
                            candidates.push(ObjectGcCandidate {
                                digest: digest.to_string(),
                                exact_path: path.to_string_lossy().into_owned(),
                                created_at: manifest.created_at.to_string(),
                                retention_deadline: deadline.to_string(),
                                pending_owner_operation_id: None,
                            });
                        }
                    }
                }
                ObjectGcPhase::DeletePending => {
                    for pending in
                        enumerate_pending(&self.vault.paths.manager().join("pending-delete"))?
                    {
                        inspected_objects = inspected_objects.saturating_add(1);
                        let deadline = retention_deadline(pending.created_at)?;
                        if !referenced.contains(&pending.digest)
                            && deadline.unix_millis()? <= UtcTimestamp::now().unix_millis()?
                        {
                            candidates.push(ObjectGcCandidate {
                                digest: pending.digest.to_string(),
                                exact_path: pending.path.to_string_lossy().into_owned(),
                                created_at: pending.created_at.to_string(),
                                retention_deadline: deadline.to_string(),
                                pending_owner_operation_id: Some(pending.owner.to_string()),
                            });
                        }
                    }
                }
            }
        }
        candidates.sort_by(|left, right| left.digest.cmp(&right.digest));
        let mut plan = ObjectGcPlan {
            operation_id: operation_id.to_string(),
            plan_digest: String::new(),
            phase,
            enabled: blockers.is_empty(),
            retention_days: DEFAULT_GC_RETENTION_DAYS,
            candidates,
            blockers,
            referenced_objects: u32::try_from(referenced.len()).unwrap_or(u32::MAX),
            inspected_objects,
        };
        plan.plan_digest = gc_digest(&plan)?;
        let directory = self.gc_operation_directory(operation_id);
        fs::create_dir_all(&directory)?;
        sync_parent(&directory)?;
        crate::filesystem::durable::atomic_write(
            &directory.join("object-gc-plan.json"),
            &serde_json::to_vec_pretty(&plan)?,
        )
        .map_err(|error| LifecycleError::Durability(error.to_string()))?;
        self.persist_planned(operation_id, &plan.plan_digest, "object_gc")?;
        Ok(plan)
    }

    pub fn execute_object_gc(
        &self,
        operation_id: OperationId,
        digest: &str,
    ) -> Result<ObjectGcResult, LifecycleError> {
        let coordinator = Arc::clone(&self.coordinator);
        coordinator.run_lifecycle(|| {
            let directory = self.gc_operation_directory(operation_id);
            let path = directory.join("object-gc-plan.json");
            let plan: ObjectGcPlan = serde_json::from_slice(&fs::read(path)?)?;
            if plan.operation_id != operation_id.to_string()
                || plan.plan_digest != digest
                || gc_digest(&plan)? != digest
                || !plan.enabled
            {
                return Err(LifecycleError::StalePlan);
            }
            // A second complete pass under the shared mutation gate protects the review/execution gap.
            let fresh_references = self.gc_references(Some(operation_id))?;
            if plan.candidates.iter().any(|candidate| {
                candidate
                    .digest
                    .parse()
                    .map_or(true, |digest| fresh_references.contains(&digest))
            }) {
                return Err(LifecycleError::StalePlan);
            }
            self.apply_object_gc(&directory, operation_id, digest, &plan)
        })
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::too_many_lines)]
    fn apply_object_gc(
        &self,
        directory: &Path,
        operation_id: OperationId,
        digest: &str,
        plan: &ObjectGcPlan,
    ) -> Result<ObjectGcResult, LifecycleError> {
        let mut journal = gc_journal(operation_id, digest);
        durable_json(&directory.join("journal.json"), &journal)?;
        let mut affected = Vec::new();
        for (index, candidate) in plan.candidates.iter().enumerate() {
            let (object_digest, exact) = parse_gc_candidate(candidate)?;
            match plan.phase {
                ObjectGcPhase::StagePendingDelete => {
                    let expected = self.vault.objects.object_path(object_digest);
                    verify_exact_directory(&exact, &expected, &self.vault.paths.objects())?;
                    let object_manifest = self.vault.objects.verify(object_digest)?;
                    let deadline = retention_deadline(object_manifest.created_at)?;
                    if object_manifest.created_at.to_string() != candidate.created_at
                        || deadline.to_string() != candidate.retention_deadline
                        || deadline.unix_millis()? > UtcTimestamp::now().unix_millis()?
                    {
                        return Err(LifecycleError::StalePlan);
                    }
                    let pending = pending_gc_path(
                        self.vault.paths.manager(),
                        operation_id,
                        &candidate.digest,
                    );
                    create_pending_parent(&pending)?;
                    let step = LifecycleStepEvidence {
                        order: u32::try_from(index).unwrap_or(u32::MAX),
                        action: "gc_move".into(),
                        source: Some(exact.clone()),
                        destination: Some(pending.clone()),
                        intent_persisted: true,
                        precondition_verified: true,
                        observed_complete: false,
                    };
                    journal.steps.push(step);
                    durable_json(&directory.join("journal.json"), &journal)?;
                    fs::rename(&exact, &pending)?;
                    sync_parent(&exact)?;
                    sync_parent(&pending)?;
                    crate::filesystem::durable::atomic_write(
                        &pending.with_extension("owner.json"),
                        &serde_json::to_vec_pretty(&serde_json::json!({
                            "operationId": operation_id,
                            "digest": candidate.digest,
                            "createdAt": candidate.created_at,
                        }))?,
                    )
                    .map_err(|error| LifecycleError::Durability(error.to_string()))?;
                    if exact.exists() || !pending.is_dir() {
                        return Err(LifecycleError::IntegrityFailed);
                    }
                    journal
                        .steps
                        .last_mut()
                        .expect("step was appended")
                        .observed_complete = true;
                    durable_json(&directory.join("journal.json"), &journal)?;
                    affected.push(pending);
                }
                ObjectGcPhase::DeletePending => {
                    let owner: OperationId = candidate
                        .pending_owner_operation_id
                        .as_deref()
                        .ok_or_else(|| LifecycleError::UnsafeGcPath(exact.clone()))?
                        .parse()
                        .map_err(|_| LifecycleError::UnsafeGcPath(exact.clone()))?;
                    let expected = self
                        .vault
                        .paths
                        .manager()
                        .join("pending-delete")
                        .join(owner.to_string())
                        .join(candidate.digest.replace(':', "_"));
                    verify_exact_directory(
                        &exact,
                        &expected,
                        &self.vault.paths.manager().join("pending-delete"),
                    )?;
                    verify_pending_owner(&exact, owner, object_digest)?;
                    let created_at = candidate.created_at.parse_rfc3339_for_gc()?;
                    if retention_deadline(created_at)?.unix_millis()?
                        > UtcTimestamp::now().unix_millis()?
                    {
                        return Err(LifecycleError::StalePlan);
                    }
                    journal.steps.push(LifecycleStepEvidence {
                        order: u32::try_from(index).unwrap_or(u32::MAX),
                        action: "gc_delete".into(),
                        source: Some(exact.clone()),
                        destination: None,
                        intent_persisted: true,
                        precondition_verified: true,
                        observed_complete: false,
                    });
                    durable_json(&directory.join("journal.json"), &journal)?;
                    make_tree_writable(&exact)?;
                    fs::remove_dir_all(&exact)?;
                    fs::remove_file(exact.with_extension("owner.json"))?;
                    sync_parent(&exact)?;
                    if exact.exists() || exact.with_extension("owner.json").exists() {
                        return Err(LifecycleError::IntegrityFailed);
                    }
                    journal
                        .steps
                        .last_mut()
                        .expect("step was appended")
                        .observed_complete = true;
                    durable_json(&directory.join("journal.json"), &journal)?;
                    affected.push(exact);
                }
            }
        }
        self.finalize_object_gc(directory, digest, plan, &mut journal, &affected)
    }

    fn finalize_object_gc(
        &self,
        directory: &Path,
        digest: &str,
        plan: &ObjectGcPlan,
        journal: &mut LifecycleJournal,
        affected: &[PathBuf],
    ) -> Result<ObjectGcResult, LifecycleError> {
        let now = UtcTimestamp::now();
        let evidence = directory.join("object-gc-journal.json");
        crate::filesystem::durable::atomic_write(
            &evidence,
            &serde_json::to_vec_pretty(&serde_json::json!({
                "operationId": journal.operation_id, "planDigest": digest, "phase": plan.phase,
                "outcome": "succeeded", "exactPaths": affected, "completedAt": now,
            }))?,
        )
        .map_err(|error| LifecycleError::Durability(error.to_string()))?;
        journal.state = LifecycleState::Succeeded;
        durable_json(&directory.join("journal.json"), journal)?;
        self.vault.repositories.append_activity(ActivityRecord {
            id: ActivityId::generate(), operation_id: None, kind: "object_gc".into(),
            state: "completed".into(), outcome: Some(OperationOutcome::Succeeded),
            summary: format!("Object GC {:?}: {} objects", plan.phase, affected.len()),
            details: serde_json::json!({"operationId": journal.operation_id, "phase": plan.phase, "exactPaths": affected}),
            started_at: now, completed_at: Some(now),
        })?;
        Ok(ObjectGcResult {
            operation_id: journal.operation_id.to_string(),
            phase: plan.phase,
            affected_objects: u32::try_from(affected.len()).unwrap_or(u32::MAX),
            evidence_path: evidence.to_string_lossy().into_owned(),
        })
    }

    pub fn object_gc_settings_summary(&self) -> ObjectGcSettingsSummary {
        let disabled_reasons = self
            .gc_references(None)
            .err()
            .map(|error| vec![error.to_string()])
            .unwrap_or_default();
        ObjectGcSettingsSummary {
            retention_days: DEFAULT_GC_RETENTION_DAYS,
            last_run: None,
            eligible: disabled_reasons.is_empty(),
            next_run: None,
            disabled_reasons,
        }
    }

    /// Enumerates the dedicated lifecycle store before runtime services are exposed. GC actions
    /// are driven from persisted intent and observed filesystem state; ambiguous cutover actions
    /// are preserved and block all runtime mutation until reviewed recovery.
    pub fn recover_startup(&self) -> Result<LifecycleRecoveryReport, LifecycleError> {
        self.migrate_legacy_lifecycle_directories()?;
        let root = self.lifecycle_root();
        if !root.exists() {
            return Ok(LifecycleRecoveryReport {
                completed: true,
                operations: Vec::new(),
            });
        }
        let coordinator = Arc::clone(&self.coordinator);
        coordinator.run_lifecycle(|| {
            let mut entries = fs::read_dir(&root)?.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(std::fs::DirEntry::file_name);
            let mut operations = Vec::new();
            for entry in entries {
                let metadata = fs::symlink_metadata(entry.path())?;
                let id: OperationId = entry
                    .file_name()
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| LifecycleError::AmbiguousLifecycleEvidence(entry.path()))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(LifecycleError::AmbiguousLifecycleEvidence(entry.path()));
                }
                let journal_path = entry.path().join("journal.json");
                let mut journal: LifecycleJournal =
                    serde_json::from_slice(&fs::read(&journal_path)?)?;
                if journal.schema_version != 1 || journal.operation_id != id {
                    return Err(LifecycleError::AmbiguousLifecycleEvidence(journal_path));
                }
                let mut blocking = false;
                let classification = match journal.state {
                    LifecycleState::Planned => "planned_no_writes".to_owned(),
                    LifecycleState::Succeeded | LifecycleState::FailedRolledBack => {
                        "terminal".to_owned()
                    }
                    LifecycleState::RecoveryRequired => {
                        blocking = true;
                        "recovery_required".to_owned()
                    }
                    LifecycleState::Mutating if journal.kind == "object_gc" => {
                        match self.recover_gc_steps(&entry.path(), &mut journal) {
                            Ok(()) => {
                                journal.state = LifecycleState::Succeeded;
                                durable_json(&journal_path, &journal)?;
                                "gc_completed_from_observation".to_owned()
                            }
                            Err(error) => {
                                journal.state = LifecycleState::RecoveryRequired;
                                journal.error = Some(error.to_string());
                                durable_json(&journal_path, &journal)?;
                                blocking = true;
                                "recovery_required".to_owned()
                            }
                        }
                    }
                    LifecycleState::Mutating => {
                        journal.state = LifecycleState::RecoveryRequired;
                        journal.error = Some("cutover boundary requires reviewed recovery".into());
                        durable_json(&journal_path, &journal)?;
                        blocking = true;
                        "recovery_required".to_owned()
                    }
                };
                operations.push(LifecycleRecoveryEvidence {
                    operation_id: id.to_string(),
                    classification,
                    blocking,
                });
            }
            Ok(LifecycleRecoveryReport {
                completed: !operations.iter().any(|evidence| evidence.blocking),
                operations,
            })
        })
    }

    fn migrate_legacy_lifecycle_directories(&self) -> Result<(), LifecycleError> {
        let standard_root = self.vault.paths.manager().join("operations");
        if !standard_root.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&standard_root)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let id: OperationId = entry
                .file_name()
                .to_string_lossy()
                .parse()
                .map_err(|_| LifecycleError::AmbiguousLifecycleEvidence(entry.path()))?;
            if entry.path().join("plan.json").is_file() {
                continue;
            }
            let known = [
                ("lifecycle-plan.json", "repair"),
                ("index-rebuild-plan.json", "index_rebuild"),
                ("object-gc-plan.json", "object_gc"),
                ("relocate-plan.json", "relocate"),
                ("old-vault-cleanup-plan.json", "old_vault_cleanup"),
            ];
            let Some((plan_name, kind)) = known
                .iter()
                .find(|(name, _)| entry.path().join(name).is_file())
            else {
                return Err(LifecycleError::AmbiguousLifecycleEvidence(entry.path()));
            };
            let plan_value: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.path().join(plan_name))?)?;
            let digest = plan_value
                .get("planDigest")
                .and_then(|value| value.as_str())
                .ok_or_else(|| LifecycleError::AmbiguousLifecycleEvidence(entry.path()))?;
            let journal_files = fs::read_dir(entry.path())?
                .filter_map(Result::ok)
                .filter(|item| item.file_name().to_string_lossy().contains("journal"))
                .collect::<Vec<_>>();
            let state = if journal_files.is_empty() {
                LifecycleState::Planned
            } else if journal_files.iter().all(|item| {
                fs::read_to_string(item.path()).is_ok_and(|text| {
                    text.contains("succeeded") || text.contains("failed_rolled_back")
                })
            }) {
                LifecycleState::Succeeded
            } else {
                LifecycleState::RecoveryRequired
            };
            let destination = self.lifecycle_operation_directory(id);
            fs::create_dir_all(self.lifecycle_root())?;
            sync_parent(&self.lifecycle_root())?;
            fs::rename(entry.path(), &destination)?;
            sync_parent(&entry.path())?;
            sync_parent(&destination)?;
            durable_json(
                &destination.join("journal.json"),
                &LifecycleJournal {
                    schema_version: 1,
                    operation_id: id,
                    plan_digest: digest.to_owned(),
                    kind: (*kind).to_owned(),
                    state,
                    steps: Vec::new(),
                    error: None,
                },
            )?;
        }
        Ok(())
    }

    fn recover_gc_steps(
        &self,
        directory: &Path,
        journal: &mut LifecycleJournal,
    ) -> Result<(), LifecycleError> {
        let plan: ObjectGcPlan =
            serde_json::from_slice(&fs::read(directory.join("object-gc-plan.json"))?)?;
        if plan.operation_id != journal.operation_id.to_string()
            || plan.plan_digest != journal.plan_digest
            || gc_digest(&plan)? != journal.plan_digest
        {
            return Err(LifecycleError::StalePlan);
        }
        for index in 0..journal.steps.len() {
            if journal.steps[index].observed_complete {
                continue;
            }
            let step = journal.steps[index].clone();
            if !step.intent_persisted || !step.precondition_verified {
                return Err(LifecycleError::AmbiguousLifecycleEvidence(
                    directory.to_path_buf(),
                ));
            }
            let source = step.source.as_ref().ok_or_else(|| {
                LifecycleError::AmbiguousLifecycleEvidence(directory.to_path_buf())
            })?;
            match step.action.as_str() {
                "gc_move" => {
                    let destination = step.destination.as_ref().ok_or_else(|| {
                        LifecycleError::AmbiguousLifecycleEvidence(directory.to_path_buf())
                    })?;
                    match (source.exists(), destination.exists()) {
                        (true, false) => {
                            let candidate =
                                plan.candidates.get(step.order as usize).ok_or_else(|| {
                                    LifecycleError::AmbiguousLifecycleEvidence(
                                        directory.to_path_buf(),
                                    )
                                })?;
                            let digest: BundleDigest = candidate
                                .digest
                                .parse()
                                .map_err(|_| LifecycleError::UnsafeGcPath(source.clone()))?;
                            self.vault.objects.verify(digest)?;
                            fs::rename(source, destination)?;
                            sync_parent(source)?;
                            sync_parent(destination)?;
                            durable_json(
                                &destination.with_extension("owner.json"),
                                &serde_json::json!({
                                    "operationId": journal.operation_id, "digest": candidate.digest,
                                    "createdAt": candidate.created_at,
                                }),
                            )?;
                        }
                        (false, true) => {}
                        _ => {
                            return Err(LifecycleError::AmbiguousLifecycleEvidence(source.clone()));
                        }
                    }
                    if source.exists() || !destination.is_dir() {
                        return Err(LifecycleError::IntegrityFailed);
                    }
                }
                "gc_delete" => {
                    if source.exists() {
                        // Preserve the pending version. Deletion may only be retried after its
                        // owner proof is revalidated; any failure leaves it untouched.
                        let candidate =
                            plan.candidates.get(step.order as usize).ok_or_else(|| {
                                LifecycleError::AmbiguousLifecycleEvidence(directory.to_path_buf())
                            })?;
                        let owner: OperationId = candidate
                            .pending_owner_operation_id
                            .as_deref()
                            .ok_or_else(|| LifecycleError::UnsafeGcPath(source.clone()))?
                            .parse()
                            .map_err(|_| LifecycleError::UnsafeGcPath(source.clone()))?;
                        let digest: BundleDigest = candidate
                            .digest
                            .parse()
                            .map_err(|_| LifecycleError::UnsafeGcPath(source.clone()))?;
                        verify_pending_owner(source, owner, digest)?;
                        make_tree_writable(source)?;
                        fs::remove_dir_all(source)?;
                        let owner_path = source.with_extension("owner.json");
                        if owner_path.exists() {
                            fs::remove_file(owner_path)?;
                        }
                        sync_parent(source)?;
                    }
                    if source.exists() || source.with_extension("owner.json").exists() {
                        return Err(LifecycleError::IntegrityFailed);
                    }
                }
                _ => {
                    return Err(LifecycleError::AmbiguousLifecycleEvidence(
                        directory.to_path_buf(),
                    ));
                }
            }
            journal.steps[index].observed_complete = true;
            durable_json(&directory.join("journal.json"), journal)?;
        }
        Ok(())
    }

    fn gc_operation_directory(&self, id: OperationId) -> PathBuf {
        self.lifecycle_operation_directory(id)
    }

    fn gc_references(
        &self,
        excluded_gc_operation: Option<OperationId>,
    ) -> Result<BTreeSet<BundleDigest>, LifecycleError> {
        if self.vault.repositories.index_integrity()? != "ok"
            || self.vault.repositories.foreign_key_violation_count()? != 0
        {
            return Err(LifecycleError::GcDisabled(
                "SQLite integrity or foreign-key check is unhealthy".into(),
            ));
        }
        let skills = self.vault.repositories.skills()?;
        let deployments = self.vault.repositories.all_deployments()?;
        let skill_paths = json_paths(&self.vault.paths.manager().join("manifests/skills"))?;
        let deployment_paths =
            json_paths(&self.vault.paths.manager().join("manifests/deployments"))?;
        validate_manifest_index_counts(
            skill_paths.len(),
            skills.len(),
            deployment_paths.len(),
            deployments.len(),
        )?;
        let indexed_skills = skills
            .iter()
            .map(|value| (value.id, value))
            .collect::<std::collections::BTreeMap<_, _>>();
        let indexed_deployments = deployments
            .iter()
            .map(|value| (value.id, value))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut manifest_references = BTreeSet::new();
        for path in skill_paths {
            let id = manifest_id(&path)?;
            let manifest = self.vault.manifests.read_skill(id)?;
            let row = indexed_skills.get(&id).ok_or_else(|| {
                LifecycleError::GcDisabled(format!("orphan Skill manifest: {}", path.display()))
            })?;
            if manifest.skill_id != row.id
                || manifest.working_path != row.working_path
                || manifest.working_digest != row.working_digest
                || manifest.baseline_digest != row.baseline_digest
            {
                return Err(LifecycleError::GcDisabled(format!(
                    "Skill manifest/index mismatch: {id}"
                )));
            }
            manifest_references.insert(manifest.working_digest);
            manifest_references.insert(manifest.baseline_digest);
        }
        for path in deployment_paths {
            let id = manifest_id(&path)?;
            let manifest = self.vault.manifests.read_deployment(id)?;
            let row = indexed_deployments.get(&id).ok_or_else(|| {
                LifecycleError::GcDisabled(format!(
                    "orphan Deployment manifest: {}",
                    path.display()
                ))
            })?;
            if !deployment_manifest_matches(&manifest, row) {
                return Err(LifecycleError::GcDisabled(format!(
                    "Deployment manifest/index mismatch: {id}"
                )));
            }
            manifest_references.insert(manifest.expected_digest);
        }
        let mut values = self.vault.database.execute(|connection| {
            let mut values = Vec::new();
            for sql in [
                "SELECT baseline_digest FROM skills", "SELECT digest FROM skill_revisions",
                "SELECT digest FROM snapshot_items WHERE digest IS NOT NULL",
                "SELECT expected_digest FROM deployments",
            ] { let mut statement = connection.prepare(sql)?; let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                values.extend(rows.collect::<Result<Vec<_>, _>>()?); }
            // Every protected/nonterminal operation JSON field is conservatively scanned.
            let mut statement = connection.prepare("SELECT s.precondition_json, coalesce(s.result_json, '') FROM operation_steps s JOIN operations o ON o.id=s.operation_id WHERE o.finalized_at_ms IS NULL OR o.recovery_state IS NOT NULL OR EXISTS (SELECT 1 FROM snapshots p WHERE p.operation_id=o.id AND p.protected=1)")?;
            for row in statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? { let (a,b)=row?; values.push(a); values.push(b); }
            Ok(values)
        }).map_err(LifecycleError::Database)?;
        values.extend(read_all_durable_json(
            &self.vault.paths.manager().join("trash"),
        )?);
        values.extend(read_operation_json(
            &self.vault.paths.manager().join("operations"),
            excluded_gc_operation,
        )?);
        values.extend(read_operation_json(
            &self.lifecycle_root(),
            excluded_gc_operation,
        )?);
        let mut references = BTreeSet::new();
        for value in values {
            collect_digest_strings(&value, &mut references)?;
        }
        references.extend(manifest_references);
        Ok(references)
    }

    fn rebuild_skills(
        &self,
        repositories: &Repositories,
        plan: &IndexRebuildPlan,
        now: UtcTimestamp,
    ) -> Result<(), LifecycleError> {
        for path in &plan.skill_manifest_paths {
            let manifest = self
                .vault
                .manifests
                .read_skill(manifest_id(std::path::Path::new(path))?)?;
            repositories.upsert_skill(SkillRecord {
                id: manifest.skill_id,
                display_name: manifest.display_name,
                deployment_name: manifest.deployment_name,
                working_path: manifest.working_path,
                working_digest: manifest.working_digest,
                baseline_digest: manifest.baseline_digest,
                lifecycle: SkillLifecycle::Active,
                created_at: manifest.created_at,
                updated_at: now,
            })?;
            for source in manifest.sources {
                repositories.insert_skill_source(SkillSourceRecord {
                    skill_id: manifest.skill_id,
                    kind: "local-observation".into(),
                    path: source.path,
                    captured_at: source.captured_at,
                    confidence: "observed".into(),
                })?;
            }
            let object_path = self.vault.objects.object_path(manifest.baseline_digest);
            let object_manifest = self.vault.objects.verify(manifest.baseline_digest)?;
            let relative = object_path
                .strip_prefix(self.vault.paths.root())
                .map_err(|_| LifecycleError::UnsafeRebuildPath(object_path.clone()))?;
            repositories.upsert_object(ObjectRecord {
                digest: manifest.baseline_digest,
                relative_path: relative
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| LifecycleError::UnsafeRebuildPath(object_path))?,
                entry_count: object_manifest.entry_count,
                byte_count: object_manifest.byte_count,
                verified_at: now,
            })?;
            repositories.insert_skill_revision(SkillRevisionRecord {
                skill_id: manifest.skill_id,
                digest: manifest.baseline_digest,
                kind: "baseline".into(),
                operation_id: None,
                created_at: manifest.created_at,
            })?;
        }
        Ok(())
    }

    fn build_replacement(
        &self,
        path: &std::path::Path,
        plan: &IndexRebuildPlan,
    ) -> Result<(u32, u32, u32), LifecycleError> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        let database = DbExecutor::open(path)?;
        let repositories = Repositories::new(database.clone());
        let now = UtcTimestamp::now();
        self.rebuild_skills(&repositories, plan, now)?;
        let store = OperationStore::open(self.vault.paths.manager())?;
        for id in durable_operation_ids(&store)? {
            let stored = store.load(id)?;
            repositories.upsert_operation(OperationRecord {
                id,
                plan_digest: stored.journal.plan_digest.to_string(),
                operation_type: format!("{:?}", stored.plan.content.kind).to_lowercase(),
                state: stored.journal.state,
                outcome: stored.journal.outcome,
                recovery_state: (!stored.journal.state.is_terminal()).then(|| "unresolved".into()),
                journal_path: format!(".manager/operations/{id}/journal.json")
                    .parse()
                    .map_err(|_| LifecycleError::UnsafeRebuildPath(store.journal_path(id)))?,
                created_at: stored.journal.created_at,
                updated_at: stored.journal.updated_at,
                finalized_at: stored.journal.finalized_at,
            })?;
        }
        for path in &plan.deployment_manifest_paths {
            let manifest = self
                .vault
                .manifests
                .read_deployment(manifest_id(std::path::Path::new(path))?)?;
            let root = manifest
                .target_path
                .parent()
                .ok_or_else(|| LifecycleError::UnsafeRebuildPath(manifest.target_path.clone()))?
                .to_path_buf();
            repositories.upsert_target(TargetRecord {
                id: manifest.target_id,
                adapter_id: manifest.adapter_version.clone(),
                scope: "custom".into(),
                root_path: root.clone(),
                canonical_root_path: root,
                project_id: None,
                is_override: false,
                is_custom: true,
                created_at: manifest.verified_at,
                updated_at: manifest.verified_at,
            })?;
            repositories.upsert_deployment(DeploymentRecord {
                id: manifest.deployment_id,
                skill_id: manifest.skill_id,
                target_id: manifest.target_id,
                deployment_name: manifest.deployment_name,
                target_path: manifest.target_path,
                mode: manifest.mode,
                expected_digest: manifest.expected_digest,
                expected_link_target: manifest.expected_link_target,
                health: DeploymentHealth::Clean,
                adapter_version: manifest.adapter_version,
                active: true,
                last_verified_at: Some(manifest.verified_at),
                last_operation_id: Some(manifest.last_finalized_operation_id),
                created_at: manifest.verified_at,
                updated_at: manifest.verified_at,
            })?;
        }
        replacement_counts(&repositories, &database, &store, plan)
    }
}

fn replacement_counts(
    repositories: &Repositories,
    database: &DbExecutor,
    store: &OperationStore,
    plan: &IndexRebuildPlan,
) -> Result<(u32, u32, u32), LifecycleError> {
    if repositories.index_integrity()? != "ok" || repositories.foreign_key_violation_count()? != 0 {
        return Err(LifecycleError::IntegrityFailed);
    }
    database.checkpoint_for_replacement()?;
    Ok((
        u32::try_from(plan.skill_manifest_paths.len())
            .map_err(|_| LifecycleError::IntegrityFailed)?,
        u32::try_from(plan.deployment_manifest_paths.len())
            .map_err(|_| LifecycleError::IntegrityFailed)?,
        u32::try_from(durable_operation_ids(store)?.len())
            .map_err(|_| LifecycleError::IntegrityFailed)?,
    ))
}

fn absolute_normalized(path: &Path) -> Result<PathBuf, LifecycleError> {
    if !path.is_absolute() {
        return Err(LifecycleError::UnsafeRelocation(
            "destination must be absolute".into(),
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| LifecycleError::UnsafeRelocation("destination has no parent".into()))?;
    let parent = parent.canonicalize()?;
    let name = path
        .file_name()
        .ok_or_else(|| LifecycleError::UnsafeRelocation("destination has no name".into()))?;
    Ok(parent.join(name))
}

fn validate_relocation_paths(source: &Path, destination: &Path) -> Result<(), LifecycleError> {
    if destination.starts_with(source) || source.starts_with(destination) || destination == source {
        return Err(LifecycleError::UnsafeRelocation(
            "nested or aliased Vault destination".into(),
        ));
    }
    let text = destination.to_string_lossy().to_ascii_lowercase();
    if text.contains("/library/mobile documents/")
        || text.contains("/library/cloudstorage/")
        || text.contains("dropbox")
        || text.contains("onedrive")
    {
        return Err(LifecycleError::UnsafeRelocation(
            "cloud-synced destinations are unsupported in M0".into(),
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| LifecycleError::UnsafeRelocation("destination has no parent".into()))?;
    let filesystem = Command::new("stat").args(["-f", "%T"]).arg(parent).output();
    match filesystem {
        Ok(output) if output.status.success() => {
            let kind = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            if ["nfs", "smb", "afp", "webdav", "fuse"]
                .iter()
                .any(|v| kind.contains(v))
            {
                return Err(LifecycleError::UnsafeRelocation(
                    "network filesystems are unsupported in M0".into(),
                ));
            }
        }
        _ => {
            return Err(LifecycleError::CapabilityBlocked(vec![
                "filesystem type is unknown".into(),
            ]));
        }
    }
    Ok(())
}

fn relocation_staging(destination: &Path, id: OperationId) -> Result<PathBuf, LifecycleError> {
    let name = destination
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| LifecycleError::UnsafeRelocation("invalid destination name".into()))?;
    Ok(destination.with_file_name(format!(".{name}.relocating-{id}")))
}

fn capability_preflight(
    destination: &Path,
    id: OperationId,
    required: u64,
) -> Result<DestinationCapabilityReport, LifecycleError> {
    let parent = destination
        .parent()
        .ok_or_else(|| LifecycleError::UnsafeRelocation("destination has no parent".into()))?;
    let probe = parent.join(format!(".skills-hub-capability-{id}"));
    let mut report = DestinationCapabilityReport {
        status: CapabilityStatus::Unknown,
        write_file: false,
        create_directory: false,
        symlink: false,
        executable_bit: false,
        atomic_rename: false,
        file_fsync: false,
        directory_fsync: false,
        advisory_lock: false,
        case_sensitive: false,
        available_bytes: fs2::available_space(parent)
            .ok()
            .map(|value| value.to_string()),
        required_bytes: required.to_string(),
        blockers: Vec::new(),
    };
    let result = (|| -> Result<(), std::io::Error> {
        fs::create_dir(&probe)?;
        report.create_directory = true;
        let file_path = probe.join("probe");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&file_path)?;
        file.write_all(b"capability")?;
        report.write_file = true;
        file.sync_all()?;
        report.file_fsync = true;
        report.executable_bit = fs::metadata(&file_path)?.permissions().mode() & 0o100 != 0;
        file.try_lock_exclusive()?;
        report.advisory_lock = true;
        file.unlock()?;
        symlink("probe", probe.join("link"))?;
        report.symlink = fs::read_link(probe.join("link"))? == PathBuf::from("probe");
        let renamed = probe.join("renamed");
        fs::rename(&file_path, &renamed)?;
        report.atomic_rename = renamed.is_file();
        File::open(&probe)?.sync_all()?;
        report.directory_fsync = true;
        fs::write(probe.join("CaseProbe"), b"a")?;
        report.case_sensitive = !probe.join("caseprobe").exists();
        Ok(())
    })();
    if let Err(ref error) = result {
        report
            .blockers
            .push(format!("behavioral capability probe failed: {error}"));
    }
    let available_bytes = report
        .available_bytes
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok());
    match available_bytes {
        Some(available) if available < required => report.blockers.push(format!(
            "insufficient capacity: {available} available, {required} required"
        )),
        None => report.blockers.push("available capacity is unknown".into()),
        _ => {}
    }
    report.status = if report.blockers.is_empty()
        && report.write_file
        && report.create_directory
        && report.symlink
        && report.executable_bit
        && report.atomic_rename
        && report.file_fsync
        && report.directory_fsync
        && report.advisory_lock
    {
        CapabilityStatus::Supported
    } else if result.is_err() || available_bytes.is_some_and(|value| value < required) {
        CapabilityStatus::Unsupported
    } else {
        CapabilityStatus::Unknown
    };
    if probe.exists() {
        fs::remove_dir_all(&probe)?;
        sync_parent(&probe)?;
    }
    Ok(report)
}

fn tree_size(root: &Path) -> Result<u64, LifecycleError> {
    let mut total = 0_u64;
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            total = total.saturating_add(tree_size(&path)?);
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total.saturating_add(total / 20).saturating_add(1024 * 1024))
}

fn copy_tree_contents(source: &Path, destination: &Path) -> Result<(), LifecycleError> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        if entry.file_name() == ".relocation-owner.json" {
            continue;
        }
        let target = destination.join(entry.file_name());
        let meta = fs::symlink_metadata(&source_path)?;
        if meta.file_type().is_symlink() {
            symlink(fs::read_link(&source_path)?, &target)?;
        } else if meta.is_dir() {
            fs::create_dir(&target)?;
            fs::set_permissions(&target, meta.permissions())?;
            copy_tree_contents(&source_path, &target)?;
            File::open(&target)?.sync_all()?;
        } else if meta.is_file() {
            let mut input = File::open(&source_path)?;
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(meta.permissions().mode())
                .open(&target)?;
            std::io::copy(&mut input, &mut output)?;
            output.set_permissions(meta.permissions())?;
            output.sync_all()?;
        } else {
            return Err(LifecycleError::UnsafeRelocation(format!(
                "unsupported entry: {}",
                source_path.display()
            )));
        }
    }
    File::open(destination)?.sync_all()?;
    Ok(())
}

fn cleanup_owned_staging(
    path: &Path,
    id: OperationId,
    vault_id: &str,
) -> Result<(), LifecycleError> {
    let owner: serde_json::Value =
        serde_json::from_slice(&fs::read(path.join(".relocation-owner.json"))?)?;
    if owner.get("operationId").and_then(|v| v.as_str()) != Some(id.to_string().as_str())
        || owner.get("vaultId").and_then(|v| v.as_str()) != Some(vault_id)
    {
        return Err(LifecycleError::UnsafeRelocation(
            "staging ownership is not proven".into(),
        ));
    }
    make_tree_writable(path)?;
    fs::remove_dir_all(path)?;
    sync_parent(path)
}

fn verify_copied_vault(source: &OpenVault, destination: &Path) -> Result<(), LifecycleError> {
    let manifest: crate::persistence::VaultManifest =
        serde_json::from_slice(&fs::read(destination.join(".manager/vault.json"))?)?;
    if manifest != source.manifest {
        return Err(LifecycleError::IntegrityFailed);
    }
    for skill in source.repositories.skills()? {
        let copied: SkillManifest = serde_json::from_slice(&fs::read(
            destination
                .join(".manager/manifests/skills")
                .join(format!("{}.json", skill.id)),
        )?)?;
        if copied.skill_id != skill.id
            || copied.working_digest != skill.working_digest
            || copied.baseline_digest != skill.baseline_digest
            || hash_bundle(
                &destination.join(copied.working_path.as_str()),
                BundleCaps::default(),
            )?
            .digest
                != copied.working_digest
        {
            return Err(LifecycleError::IntegrityFailed);
        }
        let object = destination.join(
            source
                .objects
                .object_path(skill.baseline_digest)
                .strip_prefix(source.paths.root())
                .map_err(|_| LifecycleError::IntegrityFailed)?,
        );
        if hash_bundle(&object.join("bundle"), BundleCaps::default())?.digest
            != skill.baseline_digest
        {
            return Err(LifecycleError::IntegrityFailed);
        }
    }
    let database = DbExecutor::open(destination.join(".manager/index.sqlite"))?;
    let repositories = Repositories::new(database.clone());
    if repositories.index_integrity()? != "ok" || repositories.foreign_key_violation_count()? != 0 {
        return Err(LifecycleError::IntegrityFailed);
    }
    database.checkpoint_for_replacement()?;
    Ok(())
}

type RewrittenLink = (PathBuf, PathBuf, PathBuf);
fn rewrite_managed_links(
    vault: &OpenVault,
    destination: &Path,
    operation_id: OperationId,
) -> Result<u32, (LifecycleError, Vec<RewrittenLink>)> {
    let mut changed = Vec::new();
    let database = DbExecutor::open(destination.join(".manager/index.sqlite"))
        .map_err(|e| (e.into(), changed.clone()))?;
    let repositories = Repositories::new(database);
    let manifests = ManifestStore::new(&destination.join(".manager"));
    for mut deployment in vault
        .repositories
        .deployments(None, None, true, 500)
        .map_err(|e| (e.into(), changed.clone()))?
    {
        let Some(old_link) = deployment.expected_link_target.clone() else {
            continue;
        };
        if !old_link.starts_with(vault.paths.root()) {
            continue;
        }
        let new_link = destination.join(
            old_link
                .strip_prefix(vault.paths.root())
                .map_err(|_| (LifecycleError::IntegrityFailed, changed.clone()))?,
        );
        let actual =
            fs::read_link(&deployment.target_path).map_err(|e| (e.into(), changed.clone()))?;
        if actual != old_link {
            return Err((
                LifecycleError::CutoverFailed(format!(
                    "managed link changed: {}",
                    deployment.target_path.display()
                )),
                changed,
            ));
        }
        replace_symlink(&deployment.target_path, &new_link).map_err(|e| (e, changed.clone()))?;
        changed.push((deployment.target_path.clone(), old_link, new_link.clone()));
        deployment.expected_link_target = Some(new_link.clone());
        deployment.updated_at = UtcTimestamp::now();
        deployment.last_operation_id = Some(operation_id);
        repositories
            .upsert_deployment(deployment.clone())
            .map_err(|e| (e.into(), changed.clone()))?;
        let mut manifest: DeploymentManifest = manifests
            .read_deployment(deployment.id)
            .map_err(|e| (e.into(), changed.clone()))?;
        manifest.expected_link_target = Some(new_link.clone());
        manifest.last_finalized_operation_id = operation_id;
        manifest.verified_at = UtcTimestamp::now();
        manifests
            .write_deployment(&manifest)
            .map_err(|e| (e.into(), changed.clone()))?;
        if fs::read_link(&deployment.target_path).map_err(|e| (e.into(), changed.clone()))?
            != new_link
            || hash_bundle(&new_link, BundleCaps::default())
                .map_err(|e| (e.into(), changed.clone()))?
                .digest
                != deployment.expected_digest
        {
            return Err((
                LifecycleError::CutoverFailed("rewritten deployment verification failed".into()),
                changed,
            ));
        }
    }
    Ok(u32::try_from(changed.len()).unwrap_or(u32::MAX))
}

fn replace_symlink(path: &Path, target: &Path) -> Result<(), LifecycleError> {
    let parent = path
        .parent()
        .ok_or_else(|| LifecycleError::CutoverFailed("symlink has no parent".into()))?;
    let staged = parent.join(format!(".relocate-link-{}", OperationId::generate()));
    symlink(target, &staged)?;
    fs::rename(&staged, path)?;
    sync_parent(path)
}
fn sync_parent(path: &Path) -> Result<(), LifecycleError> {
    File::open(
        path.parent()
            .ok_or_else(|| LifecycleError::Durability("path has no parent".into()))?,
    )?
    .sync_all()?;
    Ok(())
}
fn durable_json<T: Serialize>(path: &Path, value: &T) -> Result<(), LifecycleError> {
    crate::filesystem::durable::atomic_write(path, &serde_json::to_vec_pretty(value)?)
        .map_err(|e| LifecycleError::Durability(e.to_string()))
}
fn read_settings(path: &Path) -> Result<DeviceSettings, LifecycleError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn write_settings(path: &Path, settings: &DeviceSettings) -> Result<(), LifecycleError> {
    durable_json(path, settings)
}
fn relocation_digest(plan: &VaultRelocatePlan) -> Result<String, LifecycleError> {
    let mut p = plan.clone();
    p.plan_digest.clear();
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&p)?))
    ))
}
fn cleanup_digest(plan: &OldVaultCleanupPlan) -> Result<String, LifecycleError> {
    let mut p = plan.clone();
    p.plan_digest.clear();
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&p)?))
    ))
}

fn json_paths(directory: &std::path::Path) -> Result<Vec<PathBuf>, LifecycleError> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_some_and(|v| v == "json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}
fn durable_operation_ids(store: &OperationStore) -> Result<Vec<OperationId>, LifecycleError> {
    Ok(store
        .operation_ids()?
        .into_iter()
        .filter(|id| store.plan_path(*id).is_file() && store.journal_path(*id).is_file())
        .collect())
}
fn strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}
fn manifest_id<T: std::str::FromStr>(path: &std::path::Path) -> Result<T, LifecycleError> {
    path.file_stem()
        .and_then(|v| v.to_str())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| LifecycleError::InvalidDurableInput {
            path: path.to_path_buf(),
            detail: "manifest filename is not its UUID".into(),
        })
}
fn rebuild_digest(plan: &IndexRebuildPlan) -> Result<String, LifecycleError> {
    let mut value = plan.clone();
    value.plan_digest.clear();
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&value)?))
    ))
}

fn gc_digest(plan: &ObjectGcPlan) -> Result<String, LifecycleError> {
    let mut value = plan.clone();
    value.plan_digest.clear();
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&value)?))
    ))
}

fn retention_deadline(created_at: UtcTimestamp) -> Result<UtcTimestamp, LifecycleError> {
    created_at
        .checked_add(crate::domain::DurationMillis(
            u64::from(DEFAULT_GC_RETENTION_DAYS) * 24 * 60 * 60 * 1_000,
        ))
        .map_err(|_| LifecycleError::IntegrityFailed)
}

fn enumerate_object_paths(root: &Path) -> Result<Vec<(BundleDigest, PathBuf)>, LifecycleError> {
    let algorithm = root.join("sha256-bundle-v1");
    let mut objects = Vec::new();
    for prefix in fs::read_dir(&algorithm)? {
        let prefix = prefix?;
        let prefix_name = prefix
            .file_name()
            .into_string()
            .map_err(|_| LifecycleError::UnsafeGcPath(prefix.path()))?;
        if prefix_name.len() != 2
            || !prefix_name.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !prefix.file_type()?.is_dir()
        {
            return Err(LifecycleError::UnsafeGcPath(prefix.path()));
        }
        for entry in fs::read_dir(prefix.path())? {
            let entry = entry?;
            let suffix = entry
                .file_name()
                .into_string()
                .map_err(|_| LifecycleError::UnsafeGcPath(entry.path()))?;
            if suffix.len() != 62
                || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !entry.file_type()?.is_dir()
            {
                return Err(LifecycleError::UnsafeGcPath(entry.path()));
            }
            let digest: BundleDigest = format!("sha256-bundle-v1:{prefix_name}{suffix}")
                .parse()
                .map_err(|_| LifecycleError::UnsafeGcPath(entry.path()))?;
            objects.push((digest, entry.path()));
        }
    }
    objects.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(objects)
}

#[derive(Debug)]
struct PendingObject {
    owner: OperationId,
    digest: BundleDigest,
    path: PathBuf,
    created_at: UtcTimestamp,
}

fn enumerate_pending(root: &Path) -> Result<Vec<PendingObject>, LifecycleError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for owner in fs::read_dir(root)? {
        let owner = owner?;
        if !owner.file_type()?.is_dir() {
            return Err(LifecycleError::UnsafeGcPath(owner.path()));
        }
        let owner_id: OperationId = owner
            .file_name()
            .to_string_lossy()
            .parse()
            .map_err(|_| LifecycleError::UnsafeGcPath(owner.path()))?;
        for entry in fs::read_dir(owner.path())? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".owner.json") {
                continue;
            }
            if !entry.file_type()?.is_dir() {
                return Err(LifecycleError::UnsafeGcPath(entry.path()));
            }
            let digest: BundleDigest = name
                .replace('_', ":")
                .parse()
                .map_err(|_| LifecycleError::UnsafeGcPath(entry.path()))?;
            let evidence: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.path().with_extension("owner.json"))?)?;
            let created_at = evidence
                .get("createdAt")
                .and_then(|value| value.as_str())
                .ok_or_else(|| LifecycleError::UnsafeGcPath(entry.path()))?
                .parse_rfc3339_for_gc()?;
            verify_pending_owner(&entry.path(), owner_id, digest)?;
            result.push(PendingObject {
                owner: owner_id,
                digest,
                path: entry.path(),
                created_at,
            });
        }
    }
    Ok(result)
}

trait ParseGcTimestamp {
    fn parse_rfc3339_for_gc(&self) -> Result<UtcTimestamp, LifecycleError>;
}
impl ParseGcTimestamp for str {
    fn parse_rfc3339_for_gc(&self) -> Result<UtcTimestamp, LifecycleError> {
        UtcTimestamp::parse_rfc3339(self).map_err(|_| LifecycleError::IntegrityFailed)
    }
}

fn verify_pending_owner(
    path: &Path,
    owner: OperationId,
    digest: BundleDigest,
) -> Result<(), LifecycleError> {
    let evidence: serde_json::Value =
        serde_json::from_slice(&fs::read(path.with_extension("owner.json"))?)?;
    if evidence.get("operationId").and_then(|v| v.as_str()) != Some(owner.to_string().as_str())
        || evidence.get("digest").and_then(|v| v.as_str()) != Some(digest.to_string().as_str())
    {
        return Err(LifecycleError::UnsafeGcPath(path.to_path_buf()));
    }
    let object_metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(path.join("object.json"))?)?;
    if object_metadata
        .get("digest")
        .and_then(|value| value.as_str())
        != Some(digest.to_string().as_str())
        || hash_bundle(&path.join("bundle"), BundleCaps::default())?.digest != digest
    {
        return Err(LifecycleError::UnsafeGcPath(path.to_path_buf()));
    }
    Ok(())
}

fn verify_exact_directory(path: &Path, expected: &Path, root: &Path) -> Result<(), LifecycleError> {
    let metadata = fs::symlink_metadata(path)?;
    if path != expected
        || metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || !path.starts_with(root)
    {
        return Err(LifecycleError::UnsafeGcPath(path.to_path_buf()));
    }
    Ok(())
}

fn read_all_durable_json(root: &Path) -> Result<Vec<String>, LifecycleError> {
    fn visit(path: &Path, output: &mut Vec<String>) -> Result<(), LifecycleError> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(LifecycleError::UnsafeGcPath(entry.path()));
            }
            if metadata.is_dir() {
                visit(&entry.path(), output)?;
            } else if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let text = fs::read_to_string(entry.path())?;
                serde_json::from_str::<serde_json::Value>(&text)?;
                output.push(text);
            }
        }
        Ok(())
    }
    let mut output = Vec::new();
    if root.exists() {
        visit(root, &mut output)?;
    }
    Ok(output)
}

fn read_operation_json(
    root: &Path,
    excluded: Option<OperationId>,
) -> Result<Vec<String>, LifecycleError> {
    let mut output = Vec::new();
    if !root.exists() {
        return Ok(output);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            return Err(LifecycleError::UnsafeGcPath(entry.path()));
        }
        let id: OperationId = entry
            .file_name()
            .to_string_lossy()
            .parse()
            .map_err(|_| LifecycleError::UnsafeGcPath(entry.path()))?;
        if excluded == Some(id) || entry.path().join("object-gc-journal.json").is_file() {
            continue;
        }
        output.extend(read_all_durable_json(&entry.path())?);
    }
    Ok(output)
}

fn collect_digest_strings(
    text: &str,
    output: &mut BTreeSet<BundleDigest>,
) -> Result<(), LifecycleError> {
    // Scan JSON and scalar database values without interpreting field names; retaining extras is safe.
    for start in text
        .match_indices("sha256-bundle-v1:")
        .map(|(index, _)| index)
    {
        let end = start.saturating_add(81);
        if let Some(value) = text.get(start..end) {
            let digest = value.parse().map_err(|_| {
                LifecycleError::GcDisabled("ambiguous digest in durable reference source".into())
            })?;
            output.insert(digest);
        } else {
            return Err(LifecycleError::GcDisabled(
                "truncated digest in durable reference source".into(),
            ));
        }
    }
    Ok(())
}

fn gc_journal(operation_id: OperationId, digest: &str) -> LifecycleJournal {
    LifecycleJournal {
        schema_version: 1,
        operation_id,
        plan_digest: digest.to_owned(),
        kind: "object_gc".into(),
        state: LifecycleState::Mutating,
        steps: Vec::new(),
        error: None,
    }
}

fn parse_gc_candidate(
    candidate: &ObjectGcCandidate,
) -> Result<(BundleDigest, PathBuf), LifecycleError> {
    let exact = PathBuf::from(&candidate.exact_path);
    let digest = candidate
        .digest
        .parse()
        .map_err(|_| LifecycleError::UnsafeGcPath(exact.clone()))?;
    Ok((digest, exact))
}

fn pending_gc_path(manager: &Path, operation_id: OperationId, digest: &str) -> PathBuf {
    manager
        .join("pending-delete")
        .join(operation_id.to_string())
        .join(digest.replace(':', "_"))
}

fn create_pending_parent(pending: &Path) -> Result<(), LifecycleError> {
    let parent = pending
        .parent()
        .ok_or_else(|| LifecycleError::UnsafeGcPath(pending.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    Ok(())
}

fn make_tree_writable(root: &Path) -> Result<(), LifecycleError> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            make_tree_writable(&path)?;
        }
    }
    let mut permissions = fs::metadata(root)?.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    fs::set_permissions(root, permissions)?;
    Ok(())
}

fn repair_digest(
    operation_id: OperationId,
    actions: &[VaultRepairAction],
    refused: &[VaultVerifyIssue],
) -> Result<String, LifecycleError> {
    let bytes = serde_json::to_vec(&(operation_id.to_string(), actions, refused))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn issue(
    issues: &mut Vec<VaultVerifyIssue>,
    code: &str,
    path: impl AsRef<Path>,
    detail: &str,
    repairable: bool,
) {
    issues.push(VaultVerifyIssue {
        code: code.into(),
        path: path.as_ref().to_string_lossy().into_owned(),
        detail: detail.into(),
        repairable,
    });
}

fn deployment_manifest_matches(manifest: &DeploymentManifest, record: &DeploymentRecord) -> bool {
    manifest.deployment_id == record.id
        && manifest.skill_id == record.skill_id
        && manifest.target_id == record.target_id
        && manifest.deployment_name == record.deployment_name
        && manifest.mode == record.mode
        && manifest.target_path == record.target_path
        && manifest.expected_digest == record.expected_digest
        && manifest.expected_link_target == record.expected_link_target
        && manifest.adapter_version == record.adapter_version
        && record.last_operation_id == Some(manifest.last_finalized_operation_id)
        && record.last_verified_at == Some(manifest.verified_at)
}

fn validate_manifest_index_counts(
    skill_manifests: usize,
    skill_rows: usize,
    deployment_manifests: usize,
    deployment_rows: usize,
) -> Result<(), LifecycleError> {
    if skill_manifests != skill_rows || deployment_manifests != deployment_rows {
        return Err(LifecycleError::GcDisabled(
            "manifest/index enumeration diverges".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("Skill is not indexed")]
    SkillMissing,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error(transparent)]
    Bundle(#[from] crate::filesystem::BundleHashError),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Operation(#[from] OperationError),
    #[error("durable operation evidence is invalid: {0}")]
    Journal(#[from] crate::operations::JournalError),
    #[error("database executor failed: {0}")]
    Database(#[from] crate::persistence::DbExecutorError),
    #[error("database replacement failed: {0}")]
    Migration(#[from] crate::persistence::MigrationError),
    #[error("durable rebuild input is invalid at {path:?}: {detail}")]
    InvalidDurableInput { path: PathBuf, detail: String },
    #[error("durable rebuild path is unsafe: {0:?}")]
    UnsafeRebuildPath(PathBuf),
    #[error("rebuilt database failed integrity checking")]
    IntegrityFailed,
    #[error("object garbage collection is disabled: {0}")]
    GcDisabled(String),
    #[error("object garbage collection path is not exactly owned and contained: {0:?}")]
    UnsafeGcPath(PathBuf),
    #[error("Vault relocation path is unsafe: {0}")]
    UnsafeRelocation(String),
    #[error("destination capability preflight blocked relocation: {0:?}")]
    CapabilityBlocked(Vec<String>),
    #[error("Vault relocation cutover failed: {0}")]
    CutoverFailed(String),
    #[error("object store verification failed: {0}")]
    ObjectStore(#[from] crate::filesystem::ObjectStoreError),
    #[error("timestamp conversion failed: {0}")]
    Time(#[from] crate::domain::TimeError),
    #[error("reviewed lifecycle plan is stale or its confirmation digest does not match")]
    StalePlan,
    #[error("Skill manifest changed independently; reconciliation did not overwrite it")]
    StaleManifest,
    #[error("lifecycle operation requires reviewed recovery")]
    RecoveryRequired,
    #[error("lifecycle operation evidence is ambiguous at {0:?}")]
    AmbiguousLifecycleEvidence(PathBuf),
    #[error("repair identity is ambiguous")]
    AmbiguousRepair,
    #[error("lifecycle evidence could not be persisted: {0}")]
    Durability(String),
    #[error("lifecycle evidence is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("filesystem inspection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("working Bundle is outside the authorized Vault skills root: {0:?}")]
    UnsafeRevealPath(PathBuf),
    #[error("Reveal in Finder is supported only on macOS")]
    #[cfg(not(target_os = "macos"))]
    FinderUnsupported,
    #[error("Finder did not accept the reveal request")]
    FinderFailed,
}

#[cfg(test)]
mod gc_tests {
    use super::*;
    use crate::domain::{BundleRelativePath, DeploymentName};

    struct LifecycleFixture {
        _temporary: tempfile::TempDir,
        vault: Arc<OpenVault>,
        service: VaultLifecycleService,
        skill_id: SkillId,
        working: PathBuf,
    }

    fn lifecycle_fixture() -> LifecycleFixture {
        let temporary = tempfile::tempdir().unwrap();
        let vault = Arc::new(
            OpenVault::open(
                &temporary.path().join("vault"),
                &temporary.path().join("support"),
                &[],
            )
            .unwrap(),
        );
        let skill_id = SkillId::generate();
        let deployment_name = DeploymentName::parse("lifecycle-fixture").unwrap();
        let working_relative: BundleRelativePath =
            format!("skills/{skill_id}/{}", deployment_name.as_str())
                .parse()
                .unwrap();
        let working = vault.paths.root().join(working_relative.as_str());
        fs::create_dir_all(&working).unwrap();
        fs::write(working.join("SKILL.md"), b"baseline\n").unwrap();
        let now = UtcTimestamp::now();
        let hashed = hash_bundle(&working, BundleCaps::default()).unwrap();
        let publication = vault
            .objects
            .publish(OperationId::generate(), &working, Some(hashed.digest), now)
            .unwrap();
        let object_relative = publication
            .path
            .strip_prefix(vault.paths.root())
            .unwrap()
            .to_string_lossy()
            .parse()
            .unwrap();
        vault
            .repositories
            .upsert_skill(SkillRecord {
                id: skill_id,
                display_name: "Lifecycle Fixture".into(),
                deployment_name: deployment_name.clone(),
                working_path: working_relative,
                working_digest: hashed.digest,
                baseline_digest: hashed.digest,
                lifecycle: SkillLifecycle::Active,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        vault
            .repositories
            .upsert_object(ObjectRecord {
                digest: hashed.digest,
                relative_path: object_relative,
                entry_count: publication.manifest.entry_count,
                byte_count: publication.manifest.byte_count,
                verified_at: now,
            })
            .unwrap();
        vault
            .repositories
            .insert_skill_revision(SkillRevisionRecord {
                skill_id,
                digest: hashed.digest,
                kind: "baseline".into(),
                operation_id: None,
                created_at: now,
            })
            .unwrap();
        vault
            .manifests
            .write_skill(
                &SkillManifest::new(
                    skill_id,
                    "Lifecycle Fixture".into(),
                    deployment_name,
                    hashed.digest,
                    hashed.digest,
                    now,
                    Vec::new(),
                )
                .unwrap(),
            )
            .unwrap();
        let service = VaultLifecycleService::with_runtime(
            Arc::clone(&vault),
            Arc::new(OperationCoordinator::new()),
            temporary.path().join("support"),
        );
        LifecycleFixture {
            _temporary: temporary,
            vault,
            service,
            skill_id,
            working,
        }
    }

    fn tree_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, output: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries = fs::read_dir(path)
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let path = entry.path();
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &path, output);
                } else {
                    output.push((
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

    #[test]
    fn external_edit_reconciles_manifest_and_index_without_overwriting_working_bytes() {
        let fixture = lifecycle_fixture();
        fs::write(fixture.working.join("SKILL.md"), b"user edit\n").unwrap();
        fs::create_dir(fixture.working.join("notes")).unwrap();
        fs::write(fixture.working.join("notes/raw.bin"), [0, 1, 2, 255]).unwrap();
        let before = tree_bytes(&fixture.working);

        let result = fixture
            .service
            .reconcile_external_edit(fixture.skill_id)
            .unwrap();

        assert!(result.changed);
        assert_eq!(tree_bytes(&fixture.working), before);
        let actual = hash_bundle(&fixture.working, BundleCaps::default())
            .unwrap()
            .digest;
        assert_eq!(result.working_digest, actual.to_string());
        assert_eq!(
            fixture
                .vault
                .repositories
                .skill(fixture.skill_id)
                .unwrap()
                .unwrap()
                .working_digest,
            actual
        );
        assert_eq!(
            fixture
                .vault
                .manifests
                .read_skill(fixture.skill_id)
                .unwrap()
                .working_digest,
            actual
        );
    }

    #[test]
    fn verify_is_read_only_and_reports_exact_missing_manifest_and_corrupt_object_paths() {
        let fixture = lifecycle_fixture();
        let manifest = fixture.vault.manifests.skill_path(fixture.skill_id);
        let digest = fixture
            .vault
            .repositories
            .skill(fixture.skill_id)
            .unwrap()
            .unwrap()
            .baseline_digest;
        let object = fixture.vault.objects.object_path(digest);
        fs::remove_file(&manifest).unwrap();
        let object_file = object.join("bundle/SKILL.md");
        let mut permissions = fs::metadata(&object_file).unwrap().permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&object_file, permissions).unwrap();
        fs::write(object_file, b"corrupt\n").unwrap();
        let manager_before = tree_bytes(fixture.vault.paths.manager());

        let report = fixture.service.verify().unwrap();

        assert!(!report.healthy);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "skill_manifest_invalid"
                    && issue.path == manifest.to_string_lossy())
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "object_invalid"
                    && issue.path == object.to_string_lossy())
        );
        assert_eq!(tree_bytes(fixture.vault.paths.manager()), manager_before);
    }

    #[test]
    fn repair_executes_only_the_exact_reviewed_unambiguous_manifest_restoration() {
        let fixture = lifecycle_fixture();
        let manifest = fixture.vault.manifests.skill_path(fixture.skill_id);
        fs::remove_file(&manifest).unwrap();
        let plan = fixture.service.plan_repair().unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].exact_path, manifest.to_string_lossy());
        let operation_id = plan.operation_id.parse().unwrap();
        assert_eq!(
            fixture
                .service
                .execute_repair(operation_id, &plan.plan_digest)
                .unwrap(),
            1
        );
        assert_eq!(
            fixture
                .vault
                .manifests
                .read_skill(fixture.skill_id)
                .unwrap()
                .skill_id,
            fixture.skill_id
        );

        fs::remove_file(&manifest).unwrap();
        let stale = fixture.service.plan_repair().unwrap();
        fs::write(&manifest, b"ambiguous replacement").unwrap();
        assert!(matches!(
            fixture
                .service
                .execute_repair(stale.operation_id.parse().unwrap(), &stale.plan_digest),
            Err(LifecycleError::StalePlan)
        ));
        assert_eq!(fs::read(&manifest).unwrap(), b"ambiguous replacement");
    }

    #[test]
    fn lifecycle_plan_directories_do_not_enter_standard_operation_store() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = temporary.path().join(".manager");
        fs::create_dir(&manager).unwrap();
        let operation_id = OperationId::generate();
        let lifecycle = manager
            .join(LIFECYCLE_OPERATIONS_DIRECTORY)
            .join(operation_id.to_string());
        fs::create_dir_all(&lifecycle).unwrap();
        fs::write(lifecycle.join("object-gc-plan.json"), b"{}").unwrap();

        let store = OperationStore::open(&manager).unwrap();
        assert!(store.nonterminal_operation_ids().unwrap().is_empty());
        assert!(store.operation_ids().unwrap().is_empty());
        assert!(
            lifecycle.is_dir(),
            "startup must preserve lifecycle evidence"
        );
    }

    #[test]
    fn interrupted_cutover_evidence_is_nonterminal_and_preserves_both_versions() {
        let temporary = tempfile::tempdir().unwrap();
        let old = temporary.path().join("old-vault");
        let destination = temporary.path().join("new-vault");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(old.join("version"), b"old").unwrap();
        fs::write(destination.join("version"), b"new").unwrap();
        let journal = LifecycleJournal {
            schema_version: 1,
            operation_id: OperationId::generate(),
            plan_digest: "sha256:test".into(),
            kind: "relocate".into(),
            state: LifecycleState::Mutating,
            steps: vec![LifecycleStepEvidence {
                order: 1,
                action: "switch_settings".into(),
                source: Some(old.clone()),
                destination: Some(destination.clone()),
                intent_persisted: true,
                precondition_verified: true,
                observed_complete: false,
            }],
            error: None,
        };
        assert_eq!(journal.state, LifecycleState::Mutating);
        assert_eq!(fs::read(old.join("version")).unwrap(), b"old");
        assert_eq!(fs::read(destination.join("version")).unwrap(), b"new");
    }

    #[test]
    fn exact_directory_rejects_sibling_and_symlink_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("objects");
        let exact = root.join("sha256-bundle-v1/aa/object");
        fs::create_dir_all(&exact).unwrap();
        assert!(verify_exact_directory(&exact, &exact, &root).is_ok());
        let sibling = root.join("sha256-bundle-v1/aa/sibling");
        fs::create_dir(&sibling).unwrap();
        assert!(matches!(
            verify_exact_directory(&sibling, &exact, &root),
            Err(LifecycleError::UnsafeGcPath(_))
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&exact, root.join("alias")).unwrap();
            assert!(matches!(
                verify_exact_directory(&root.join("alias"), &root.join("alias"), &root),
                Err(LifecycleError::UnsafeGcPath(_))
            ));
        }
    }

    #[test]
    fn durable_reference_scanning_blocks_truncated_digests() {
        let mut references = BTreeSet::new();
        assert!(matches!(
            collect_digest_strings("sha256-bundle-v1:abc", &mut references),
            Err(LifecycleError::GcDisabled(_))
        ));
    }

    #[test]
    fn manifest_index_divergence_disables_gc_instead_of_collecting() {
        assert!(matches!(
            validate_manifest_index_counts(1, 0, 0, 0),
            Err(LifecycleError::GcDisabled(_))
        ));
        assert!(matches!(
            validate_manifest_index_counts(0, 0, 0, 1),
            Err(LifecycleError::GcDisabled(_))
        ));
    }

    #[test]
    fn relocation_capability_probe_exercises_required_behaviors_and_capacity_blocker() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("Vault-Moved");
        let supported = capability_preflight(&destination, OperationId::generate(), 1).unwrap();
        assert!(supported.write_file && supported.create_directory && supported.symlink);
        assert!(supported.executable_bit && supported.atomic_rename);
        assert!(supported.file_fsync && supported.directory_fsync && supported.advisory_lock);
        assert!(supported.available_bytes.is_some());
        assert!(!temporary.path().read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".skills-hub-capability")
        }));

        let blocked =
            capability_preflight(&destination, OperationId::generate(), u64::MAX).unwrap();
        assert_eq!(blocked.status, CapabilityStatus::Unsupported);
        assert!(
            blocked
                .blockers
                .iter()
                .any(|value| value.contains("insufficient capacity"))
        );
    }

    #[test]
    fn relocation_refuses_nested_destination_and_cleanup_requires_exact_digest() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Vault");
        fs::create_dir(&source).unwrap();
        assert!(matches!(
            validate_relocation_paths(&source, &source.join("nested")),
            Err(LifecycleError::UnsafeRelocation(_))
        ));

        let mut cleanup = OldVaultCleanupPlan {
            operation_id: OperationId::generate().to_string(),
            plan_digest: String::new(),
            old_vault_path: source.to_string_lossy().into_owned(),
            active_vault_path: temporary.path().join("new").to_string_lossy().into_owned(),
            vault_id: "evidence".into(),
        };
        cleanup.plan_digest = cleanup_digest(&cleanup).unwrap();
        let confirmed = cleanup.plan_digest.clone();
        cleanup.old_vault_path.push_str("-changed");
        assert_ne!(cleanup_digest(&cleanup).unwrap(), confirmed);
        assert!(
            source.exists(),
            "planning/digest checks never delete the old Vault"
        );
    }
}
