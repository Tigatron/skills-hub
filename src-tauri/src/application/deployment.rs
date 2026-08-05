use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use specta::Type;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    application::{
        activity::ActivityService,
        takeover::{DeploymentModeDto, OperationView as TakeoverOperationView},
    },
    domain::{
        ActivityId, AdapterId, BundleDigest, BundleRelativePath, DeploymentHealth, DeploymentId,
        DeploymentMode, DeploymentName, DurationMillis, ManagedTargetObservation, OperationId,
        OperationOutcome, OperationState, OperationTone, ProjectId, SkillId, SkillLifecycle,
        SnapshotId, SymlinkTargetObservation, TargetId, UtcTimestamp, managed_copy_health,
        normalized_collision_key, symlink_health,
    },
    filesystem::{
        AuthorizedRoot, BundleCaps, BundleStats, EntryKind, MetadataFingerprint, copy_bundle_exact,
        hash_bundle,
    },
    operations::{
        BatchDeploymentAction, BatchDeploymentEntryEvidence, BatchDeploymentInverseEvidence,
        BatchDeploymentPlanContext, CancellationToken, CapabilityStatus, DeploymentPlanContext,
        DeploymentProductAction, DeploymentSkillEvidence, DeploymentTargetEvidence,
        ManagedDeploymentEvidence, OperationCoordinator, OperationError, OperationExecutor,
        OperationFailpoints, OperationFinalizer, OperationHookError, OperationIntent,
        OperationKind, OperationPlan, OperationPlanContent, OperationPlanner, OperationStore,
        PathFingerprint, PlanAction, PlanBuilder, PlanPath, PlanStep, RecoverySummary,
        SnapshotProtection, SnapshotRegistrar, SnapshotRegistration, StagingProvider,
        TakeoverTargetScope, TargetCapabilityEvidence, TargetRoots, UndeployResolution,
    },
    persistence::{
        ActivityRecord, AdapterConfigurationRecord, BatchDeploymentProjection, DeploymentManifest,
        DeploymentProjection, DeploymentRecord, ManifestError, OpenVault, OperationRecord,
        ProjectRecord, Repositories, RepositoryError, SnapshotItemRecord, SnapshotRecord,
        TargetRecord, TargetRegistrationMetadataRecord,
    },
};

const UNIVERSAL_ADAPTER: &str = "universal-agent-skills@1";
const PLAN_TTL: DurationMillis = DurationMillis(300_000);
const SNAPSHOT_SCHEMA: u32 = 1;
const MAX_DEPLOYMENTS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum FixtureTargetKindDto {
    Global,
    GitProject,
    PersonalProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterTargetRequest {
    pub kind: FixtureTargetKindDto,
    pub selected_directory: String,
    pub adapter_id: Option<String>,
    pub is_override: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterConfigureRequest {
    pub adapter_id: String,
    pub enabled: bool,
    pub global_override_path: Option<String>,
    pub project_override_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterProjectTargetRegisterRequest {
    pub adapter_id: String,
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredAdapterView {
    pub adapter_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub global_override_path: Option<String>,
    pub project_override_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CustomTargetScope {
    Global,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomTargetRegisterRequest {
    pub target_id: Option<String>,
    pub display_name: String,
    pub selected_directory: String,
    pub scope: CustomTargetScope,
    pub preferred_mode: DeploymentModeDto,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TargetView {
    pub target_id: String,
    pub adapter_id: String,
    pub scope: String,
    pub project_id: Option<String>,
    pub project_kind: Option<String>,
    pub root_path: String,
    pub is_override: bool,
    pub is_custom: bool,
    pub default_mode: DeploymentModeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentPlanRequest {
    pub skill_id: String,
    pub target_id: String,
    pub requested_mode: Option<DeploymentModeDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeploymentTargetChoice {
    pub target_id: String,
    pub requested_mode: Option<DeploymentModeDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchDeploymentPlanRequest {
    pub skill_id: String,
    pub targets: Vec<BatchDeploymentTargetChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeploymentPlanView {
    pub operation_id: String,
    pub plan_digest: String,
    pub expires_at: String,
    pub action: String,
    pub skill_id: String,
    pub entries: Vec<DeploymentPlanView>,
    pub recovery_count: u32,
    pub consequence: String,
    pub execution_allowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum UndeployResolutionDto {
    RemoveManaged,
    PreserveTarget,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UndeployPlanRequest {
    pub deployment_id: String,
    pub resolution: UndeployResolutionDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPlanView {
    pub operation_id: String,
    pub plan_digest: String,
    pub expires_at: String,
    pub action: String,
    pub skill_id: String,
    pub target_id: String,
    pub deployment_id: String,
    pub target_path: String,
    pub requested_mode: DeploymentModeDto,
    pub resolved_mode: DeploymentModeDto,
    pub fallback_reason: Option<String>,
    pub reviewed_health: String,
    pub no_op: bool,
    pub consequence: String,
    pub recovery_count: u32,
    pub execution_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentIdRequest {
    pub deployment_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentQuery {
    pub skill_id: Option<String>,
    pub target_id: Option<String>,
    pub include_inactive: bool,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentHealthView {
    pub deployment_id: String,
    pub skill_id: String,
    pub target_id: String,
    pub deployment_name: String,
    pub target_path: String,
    pub mode: DeploymentModeDto,
    pub active: bool,
    pub health: String,
    pub explanation: String,
    pub expected_digest: String,
    pub vault_digest: Option<String>,
    pub target_digest: Option<String>,
    pub expected_link_target: Option<String>,
    pub actual_link_target: Option<String>,
    pub drift_direction: String,
    pub allowed_actions: Vec<String>,
    pub disabled_reason: Option<String>,
    pub verified_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentPage {
    pub items: Vec<DeploymentHealthView>,
    pub count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentOperationView {
    pub operation_id: String,
    pub plan_digest: String,
    pub state: String,
    pub outcome: Option<String>,
    pub terminal: bool,
    pub cancellation_allowed: bool,
    pub tone: OperationTone,
    pub failure: Option<String>,
    pub recovery: Vec<String>,
    pub review: DeploymentPlanView,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeploymentOperationView {
    pub operation_id: String,
    pub plan_digest: String,
    pub state: String,
    pub outcome: Option<String>,
    pub terminal: bool,
    pub cancellation_allowed: bool,
    pub tone: OperationTone,
    pub failure: Option<String>,
    pub recovery: Vec<String>,
    pub review: BatchDeploymentPlanView,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AnyOperationView {
    Takeover(TakeoverOperationView),
    Deployment(DeploymentOperationView),
    BatchDeployment(BatchDeploymentOperationView),
}

#[derive(Debug, Error)]
pub enum DeploymentError {
    #[error("invalid {entity} ID: {detail}")]
    InvalidId {
        entity: &'static str,
        detail: String,
    },
    #[error("selected target directory is not an existing safe absolute directory")]
    InvalidTargetDirectory,
    #[error("Skill does not exist or is not active")]
    SkillMissing,
    #[error("Target does not exist or its authority changed")]
    TargetMissing,
    #[error("Deployment does not exist or is inactive")]
    DeploymentMissing,
    #[error("an unmanaged or differently owned entry already occupies the deployment name")]
    UnmanagedCollision,
    #[error("target capability is unsupported or could not be proven: {0}")]
    CapabilityBlocked(String),
    #[error("deployment drift requires an explicit safe resolution: {0}")]
    DriftBlocked(String),
    #[error("deployment planning was cancelled")]
    PlanningCancelled,
    #[error("filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("manifest operation failed: {0}")]
    Manifest(#[from] ManifestError),
    #[error("persistence failed: {0}")]
    Persistence(#[from] RepositoryError),
    #[error("operation failed: {0}")]
    Operation(#[from] OperationError),
    #[error("operation evidence failed: {0}")]
    Journal(String),
}

trait CapabilityProbe: Send + Sync {
    fn inspect(
        &self,
        target_root: &Path,
        link_target: &Path,
    ) -> Result<TargetCapabilityEvidence, DeploymentError>;
}

#[derive(Debug)]
struct FilesystemCapabilityProbe;

impl CapabilityProbe for FilesystemCapabilityProbe {
    fn inspect(
        &self,
        target_root: &Path,
        link_target: &Path,
    ) -> Result<TargetCapabilityEvidence, DeploymentError> {
        let marker = Uuid::now_v7();
        let file = target_root.join(format!(".skills-hub-{marker}.cap"));
        let source = target_root.join(format!(".skills-hub-{marker}.rename-source"));
        let destination = target_root.join(format!(".skills-hub-{marker}.rename-destination"));
        let link = target_root.join(format!(".skills-hub-{marker}.link"));
        let write = match File::create(&file).and_then(|file| file.sync_all()) {
            Ok(()) => CapabilityStatus::Supported,
            Err(error) => capability_status(&error),
        };
        let rename = match fs::create_dir(&source).and_then(|()| fs::rename(&source, &destination))
        {
            Ok(()) => CapabilityStatus::Supported,
            Err(error) => capability_status(&error),
        };
        #[cfg(unix)]
        let symlink = match std::os::unix::fs::symlink(link_target, &link)
            .and_then(|()| fs::read_link(&link).map(|actual| actual == link_target))
        {
            Ok(true) => CapabilityStatus::Supported,
            Ok(false) => CapabilityStatus::Unknown,
            Err(error) => capability_status(&error),
        };
        #[cfg(not(unix))]
        let symlink = CapabilityStatus::Unsupported;
        remove_probe_path(&link);
        remove_probe_path(&destination);
        remove_probe_path(&source);
        remove_probe_path(&file);
        let _ = File::open(target_root).and_then(|directory| directory.sync_all());
        Ok(TargetCapabilityEvidence {
            directory_write: write,
            atomic_rename: rename,
            symlink,
        })
    }
}

fn capability_status(error: &io::Error) -> CapabilityStatus {
    match error.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported => {
            CapabilityStatus::Unsupported
        }
        _ => CapabilityStatus::Unknown,
    }
}

fn remove_probe_path(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let _ = fs::remove_dir(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

pub struct DeploymentService {
    vault: Arc<OpenVault>,
    coordinator: Arc<OperationCoordinator>,
    cancellations: Arc<Mutex<BTreeMap<OperationId, CancellationToken>>>,
    capability_probe: Arc<dyn CapabilityProbe>,
    operation_failpoints: Arc<dyn OperationFailpoints>,
}

impl DeploymentService {
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
            capability_probe: Arc::new(FilesystemCapabilityProbe),
            operation_failpoints: Arc::new(crate::operations::NoopOperationFailpoints),
        }
    }

    #[cfg(test)]
    fn with_test_hooks(
        mut self,
        capability_probe: Arc<dyn CapabilityProbe>,
        operation_failpoints: Arc<dyn OperationFailpoints>,
    ) -> Self {
        self.capability_probe = capability_probe;
        self.operation_failpoints = operation_failpoints;
        self
    }

    #[allow(clippy::too_many_lines)]
    pub fn register_target(
        &self,
        request: &RegisterTargetRequest,
    ) -> Result<TargetView, DeploymentError> {
        let selected = PathBuf::from(&request.selected_directory);
        let root =
            AuthorizedRoot::open(&selected).map_err(|_| DeploymentError::InvalidTargetDirectory)?;
        ensure_disjoint(self.vault.paths.root(), root.canonical_path())?;
        let now = UtcTimestamp::now();
        let adapter_id = request
            .adapter_id
            .as_deref()
            .map_or_else(adapter_id, |value| parse_id(value, "Adapter"))?;
        if !crate::adapters::is_known(&adapter_id) {
            return Err(DeploymentError::TargetMissing);
        }
        let (scope, project_id, project_kind) = match request.kind {
            FixtureTargetKindDto::Global => ("global".to_owned(), None, None),
            FixtureTargetKindDto::GitProject | FixtureTargetKindDto::PersonalProject => {
                let existing = self
                    .vault
                    .repositories
                    .targets(MAX_DEPLOYMENTS)?
                    .into_iter()
                    .find(|target| {
                        target.adapter_id == adapter_id
                            && target.scope == "project"
                            && target.canonical_root_path == root.canonical_path()
                            && target.project_id.is_some()
                    });
                if let Some(existing) = existing {
                    let project = self
                        .vault
                        .repositories
                        .project(existing.project_id.expect("filtered"))?
                        .ok_or(DeploymentError::TargetMissing)?;
                    let requested = project_kind_text(request.kind);
                    if project.git_classification != requested {
                        return Err(DeploymentError::TargetMissing);
                    }
                    return target_view(&existing, Some(&project));
                }
                let id = ProjectId::generate();
                let classification = project_kind_text(request.kind).to_owned();
                self.vault.repositories.upsert_project(ProjectRecord {
                    id,
                    workspace_root_id: None,
                    root_path: selected.clone(),
                    canonical_path: root.canonical_path().to_path_buf(),
                    discovery_evidence: "fixture_registration".to_owned(),
                    git_classification: classification.clone(),
                    manual: true,
                    created_at: now,
                    updated_at: now,
                })?;
                ("project".to_owned(), Some(id), Some(classification))
            }
        };
        if let Some(existing) = self.vault.repositories.target_by_identity(
            adapter_id.clone(),
            scope.clone(),
            project_id,
            root.canonical_path(),
        )? {
            let project = existing
                .project_id
                .map(|id| self.vault.repositories.project(id))
                .transpose()?
                .flatten();
            return target_view(&existing, project.as_ref());
        }
        let target = TargetRecord {
            id: TargetId::generate(),
            adapter_id,
            scope,
            root_path: selected,
            canonical_root_path: root.canonical_path().to_path_buf(),
            project_id,
            is_override: request.is_override.unwrap_or(false),
            is_custom: false,
            created_at: now,
            updated_at: now,
        };
        self.vault.repositories.upsert_target(target.clone())?;
        let identity = root.identity();
        self.vault
            .repositories
            .upsert_target_registration_metadata(TargetRegistrationMetadataRecord {
                target_id: target.id,
                display_name: format!("{} {}", target.adapter_id, target.scope),
                preferred_mode: None,
                root_device_id: identity.device_id,
                root_file_id: identity.file_id,
                override_kind: target.is_override.then(|| "fixture".to_owned()),
                created_at: now,
                updated_at: now,
            })?;
        let project = project_kind.map(|kind| ProjectRecord {
            id: project_id.expect("project kind has project ID"),
            workspace_root_id: None,
            root_path: target.root_path.clone(),
            canonical_path: target.canonical_root_path.clone(),
            discovery_evidence: "fixture_registration".to_owned(),
            git_classification: kind,
            manual: true,
            created_at: now,
            updated_at: now,
        });
        target_view(&target, project.as_ref())
    }

    pub fn adapters_configured_list(&self) -> Result<Vec<ConfiguredAdapterView>, DeploymentError> {
        let configured = self.vault.repositories.adapter_configurations()?;
        Ok(crate::adapters::DESCRIPTORS
            .into_iter()
            .map(|descriptor| {
                let row = configured
                    .iter()
                    .find(|row| row.adapter_name == descriptor.name);
                ConfiguredAdapterView {
                    adapter_id: descriptor.id().to_string(),
                    display_name: descriptor.display_name.to_owned(),
                    enabled: row.is_none_or(|r| r.enabled),
                    global_override_path: row
                        .and_then(|r| r.global_override_path.as_ref())
                        .map(|p| p.to_string_lossy().into_owned()),
                    project_override_path: row.and_then(|r| r.project_override_path.clone()),
                }
            })
            .collect())
    }

    pub fn configure_adapter(
        &self,
        request: &AdapterConfigureRequest,
    ) -> Result<ConfiguredAdapterView, DeploymentError> {
        let id = parse_id::<AdapterId>(&request.adapter_id, "Adapter")?;
        let descriptor = crate::adapters::descriptor(&id).ok_or(DeploymentError::TargetMissing)?;
        let project_override = request
            .project_override_path
            .as_deref()
            .map(BundleRelativePath::parse)
            .transpose()
            .map_err(|_| DeploymentError::InvalidTargetDirectory)?
            .map(|p| p.to_string());
        let now = UtcTimestamp::now();
        let global_override = if let Some(path) = &request.global_override_path {
            let root =
                AuthorizedRoot::open(path).map_err(|_| DeploymentError::InvalidTargetDirectory)?;
            ensure_disjoint(self.vault.paths.root(), root.canonical_path())?;
            let mut target = self
                .vault
                .repositories
                .target_by_identity(id.clone(), "global".into(), None, root.canonical_path())?
                .unwrap_or(TargetRecord {
                    id: TargetId::generate(),
                    adapter_id: id.clone(),
                    scope: "global".into(),
                    root_path: PathBuf::from(path),
                    canonical_root_path: root.canonical_path().to_path_buf(),
                    project_id: None,
                    is_override: true,
                    is_custom: false,
                    created_at: now,
                    updated_at: now,
                });
            target.root_path = PathBuf::from(path);
            target.canonical_root_path = root.canonical_path().to_path_buf();
            target.is_override = true;
            target.updated_at = now;
            self.vault.repositories.upsert_target(target.clone())?;
            let identity = root.identity();
            self.vault
                .repositories
                .upsert_target_registration_metadata(TargetRegistrationMetadataRecord {
                    target_id: target.id,
                    display_name: format!("{} override", descriptor.display_name),
                    preferred_mode: None,
                    root_device_id: identity.device_id,
                    root_file_id: identity.file_id,
                    override_kind: Some("global".into()),
                    created_at: now,
                    updated_at: now,
                })?;
            Some(root.canonical_path().to_path_buf())
        } else {
            None
        };
        self.vault
            .repositories
            .upsert_adapter_configuration(AdapterConfigurationRecord {
                adapter_name: descriptor.name.into(),
                adapter_id: id,
                enabled: request.enabled,
                global_override_path: global_override.clone(),
                project_override_path: project_override.clone(),
                created_at: now,
                updated_at: now,
            })?;
        Ok(ConfiguredAdapterView {
            adapter_id: request.adapter_id.clone(),
            display_name: descriptor.display_name.into(),
            enabled: request.enabled,
            global_override_path: global_override.map(|p| p.to_string_lossy().into_owned()),
            project_override_path: project_override,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub fn register_custom_target(
        &self,
        request: &CustomTargetRegisterRequest,
    ) -> Result<TargetView, DeploymentError> {
        let display = request.display_name.trim();
        if display.is_empty() {
            return Err(DeploymentError::InvalidTargetDirectory);
        }
        let root = AuthorizedRoot::open(&request.selected_directory)
            .map_err(|_| DeploymentError::InvalidTargetDirectory)?;
        ensure_disjoint(self.vault.paths.root(), root.canonical_path())?;
        let project_id = request
            .project_id
            .as_deref()
            .map(|v| parse_id::<ProjectId>(v, "Project"))
            .transpose()?;
        if let Some(id) = project_id {
            self.vault
                .repositories
                .project(id)?
                .ok_or(DeploymentError::TargetMissing)?;
        }
        let scope = match request.scope {
            CustomTargetScope::Global => "global",
            CustomTargetScope::Project => "project",
        };
        if scope == "global" && project_id.is_some() {
            return Err(DeploymentError::TargetMissing);
        }
        let adapter_id = AdapterId::new("custom-directory", 1).expect("static adapter");
        let now = UtcTimestamp::now();
        let reselected = if let Some(value) = request.target_id.as_deref() {
            let id = parse_id::<TargetId>(value, "Target")?;
            Some(
                self.vault
                    .repositories
                    .target(id)?
                    .ok_or(DeploymentError::TargetMissing)?,
            )
        } else {
            None
        };
        if reselected
            .as_ref()
            .is_some_and(|target| !target.is_custom || target.adapter_id != adapter_id)
        {
            return Err(DeploymentError::TargetMissing);
        }
        let mut target = if let Some(mut target) = reselected {
            target.scope = scope.into();
            target.project_id = project_id;
            target
        } else {
            let existing = self.vault.repositories.target_by_identity(
                adapter_id.clone(),
                scope.into(),
                project_id,
                root.canonical_path(),
            )?;
            if let Some(existing) = &existing {
                let metadata = self
                    .vault
                    .repositories
                    .target_registration_metadata(existing.id)?
                    .ok_or(DeploymentError::TargetMissing)?;
                let identity = root.identity();
                if identity.device_id != metadata.root_device_id
                    || identity.file_id != metadata.root_file_id
                {
                    return Err(DeploymentError::TargetMissing);
                }
            }
            existing.unwrap_or(TargetRecord {
                id: TargetId::generate(),
                adapter_id,
                scope: scope.into(),
                root_path: PathBuf::from(&request.selected_directory),
                canonical_root_path: root.canonical_path().to_path_buf(),
                project_id,
                is_override: false,
                is_custom: true,
                created_at: now,
                updated_at: now,
            })
        };
        target.root_path = PathBuf::from(&request.selected_directory);
        target.canonical_root_path = root.canonical_path().to_path_buf();
        target.updated_at = now;
        self.vault.repositories.upsert_target(target.clone())?;
        let identity = root.identity();
        self.vault
            .repositories
            .upsert_target_registration_metadata(TargetRegistrationMetadataRecord {
                target_id: target.id,
                display_name: display.into(),
                preferred_mode: Some(mode(request.preferred_mode)),
                root_device_id: identity.device_id,
                root_file_id: identity.file_id,
                override_kind: None,
                created_at: now,
                updated_at: now,
            })?;
        let project = project_id
            .map(|id| self.vault.repositories.project(id))
            .transpose()?
            .flatten();
        Ok(target_view_with_mode(
            &target,
            project.as_ref(),
            mode(request.preferred_mode),
        ))
    }

    pub fn register_adapter_project_target(
        &self,
        request: &AdapterProjectTargetRegisterRequest,
    ) -> Result<TargetView, DeploymentError> {
        let adapter_id = parse_id::<AdapterId>(&request.adapter_id, "Adapter")?;
        let descriptor =
            crate::adapters::descriptor(&adapter_id).ok_or(DeploymentError::TargetMissing)?;
        let project_id = parse_id::<ProjectId>(&request.project_id, "Project")?;
        let project = self
            .vault
            .repositories
            .project(project_id)?
            .ok_or(DeploymentError::TargetMissing)?;
        let configuration = self
            .vault
            .repositories
            .adapter_configurations()?
            .into_iter()
            .find(|row| row.adapter_name == descriptor.name);
        if configuration.as_ref().is_some_and(|row| !row.enabled) {
            return Err(DeploymentError::TargetMissing);
        }
        let (relative, is_override) = configuration
            .and_then(|row| row.project_override_path)
            .map_or_else(
                || (descriptor.project_path.to_owned(), false),
                |path| (path, true),
            );
        let relative = BundleRelativePath::parse(&relative)
            .map_err(|_| DeploymentError::InvalidTargetDirectory)?;
        let selected = project.canonical_path.join(relative.as_str());
        let root =
            AuthorizedRoot::open(&selected).map_err(|_| DeploymentError::InvalidTargetDirectory)?;
        ensure_disjoint(self.vault.paths.root(), root.canonical_path())?;
        let now = UtcTimestamp::now();
        let mut target = self
            .vault
            .repositories
            .target_by_identity(
                adapter_id.clone(),
                "project".into(),
                Some(project_id),
                root.canonical_path(),
            )?
            .unwrap_or(TargetRecord {
                id: TargetId::generate(),
                adapter_id,
                scope: "project".into(),
                root_path: selected.clone(),
                canonical_root_path: root.canonical_path().to_path_buf(),
                project_id: Some(project_id),
                is_override,
                is_custom: false,
                created_at: now,
                updated_at: now,
            });
        target.root_path = selected;
        target.canonical_root_path = root.canonical_path().to_path_buf();
        target.is_override = is_override;
        target.updated_at = now;
        self.vault.repositories.upsert_target(target.clone())?;
        let identity = root.identity();
        self.vault
            .repositories
            .upsert_target_registration_metadata(TargetRegistrationMetadataRecord {
                target_id: target.id,
                display_name: format!(
                    "{} — {}",
                    descriptor.display_name,
                    project.root_path.display()
                ),
                preferred_mode: None,
                root_device_id: identity.device_id,
                root_file_id: identity.file_id,
                override_kind: is_override.then(|| "project".into()),
                created_at: now,
                updated_at: now,
            })?;
        target_view(&target, Some(&project))
    }

    pub fn targets(&self) -> Result<Vec<TargetView>, DeploymentError> {
        let mut views = Vec::new();
        for target in self.vault.repositories.targets(MAX_DEPLOYMENTS)? {
            match ensure_target_is_configured(&self.vault.repositories, &target) {
                Ok(()) => {}
                Err(DeploymentError::TargetMissing) => continue,
                Err(error) => return Err(error),
            }
            let project = target
                .project_id
                .map(|id| self.vault.repositories.project(id))
                .transpose()?
                .flatten();
            let view = if target.is_custom {
                let preferred_mode = self
                    .vault
                    .repositories
                    .target_registration_metadata(target.id)?
                    .and_then(|metadata| metadata.preferred_mode)
                    .ok_or(DeploymentError::TargetMissing)?;
                target_view_with_mode(&target, project.as_ref(), preferred_mode)
            } else {
                target_view(&target, project.as_ref())?
            };
            views.push(view);
        }
        Ok(views)
    }

    #[allow(clippy::too_many_lines)]
    pub fn plan_deployment(
        &self,
        request: &DeploymentPlanRequest,
    ) -> Result<DeploymentPlanView, DeploymentError> {
        let (context, step, stats, now) = self.build_deployment(request)?;
        self.persist_plan(context, step, stats, now)
    }

    #[allow(clippy::too_many_lines)]
    fn build_deployment(
        &self,
        request: &DeploymentPlanRequest,
    ) -> Result<(DeploymentPlanContext, PlanStep, BundleStats, UtcTimestamp), DeploymentError> {
        let skill_id = parse_id::<SkillId>(&request.skill_id, "Skill")?;
        let target_id = parse_id::<TargetId>(&request.target_id, "Target")?;
        let skill = self
            .vault
            .repositories
            .skill(skill_id)?
            .filter(|skill| skill.lifecycle == SkillLifecycle::Active)
            .ok_or(DeploymentError::SkillMissing)?;
        let (target, project, root) = self.open_target(target_id)?;
        ensure_target_is_configured(&self.vault.repositories, &target)?;
        if !target.is_custom {
            let enabled = self
                .vault
                .repositories
                .adapter_configurations()?
                .into_iter()
                .find(|row| row.adapter_id == target.adapter_id)
                .is_none_or(|row| row.enabled);
            if !enabled {
                return Err(DeploymentError::TargetMissing);
            }
        }
        let working = self.vault.paths.root().join(skill.working_path.as_str());
        let current = hash_bundle(&working, BundleCaps::default())
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?;
        if current.digest != skill.working_digest {
            return Err(DeploymentError::DriftBlocked(
                "Vault working content changed after its durable Skill record".to_owned(),
            ));
        }
        let manifest = self.vault.manifests.read_skill(skill_id)?;
        if manifest.working_digest != current.digest || manifest.working_path != skill.working_path
        {
            return Err(DeploymentError::DriftBlocked(
                "Skill manifest differs from the current Vault working version".to_owned(),
            ));
        }
        let capability = self
            .capability_probe
            .inspect(root.canonical_path(), &working)?;
        require_base_capability(&capability)?;
        let default = if target.is_custom {
            self.vault
                .repositories
                .target_registration_metadata(target.id)?
                .and_then(|metadata| metadata.preferred_mode)
                .ok_or(DeploymentError::TargetMissing)?
        } else {
            default_mode(&target, project.as_ref())?
        };
        let requested = request.requested_mode.map_or(default, mode);
        let (resolved, fallback_reason) = resolve_mode(requested, &capability)?;
        let relative = BundleRelativePath::parse(skill.deployment_name.as_str())
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?;
        let authorized = root
            .authorize(&relative)
            .map_err(|_| DeploymentError::TargetMissing)?;
        ensure_no_name_collision(
            root.canonical_path(),
            &skill.deployment_name,
            authorized.path(),
        )?;
        let existing = self
            .vault
            .repositories
            .active_deployment_for_target_name(target_id, skill.deployment_name.clone())?;
        let existing_deployment = existing.is_some();
        let now = UtcTimestamp::now();
        let (
            deployment_id,
            action,
            before,
            health,
            previous_digest,
            previous_link,
            created_at,
            updated_at,
        ) = match existing {
            None => {
                let observation = authorized
                    .inspect()
                    .map_err(|_| DeploymentError::TargetMissing)?;
                if observation.kind != EntryKind::Absent {
                    return Err(DeploymentError::UnmanagedCollision);
                }
                (
                    DeploymentId::generate(),
                    PlanAction::Create,
                    fingerprint(
                        &target,
                        EntryKind::Absent,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        now,
                    ),
                    DeploymentHealth::MissingTarget,
                    None,
                    None,
                    now,
                    now,
                )
            }
            Some(existing) => {
                if existing.skill_id != skill_id || existing.target_path != authorized.path() {
                    return Err(DeploymentError::UnmanagedCollision);
                }
                let evaluated = self.evaluate(&existing, false)?;
                let previous_expected_link_target = existing
                    .expected_link_target
                    .as_deref()
                    .map(|path| exact_plan_path(path, "reviewed expected symlink target"))
                    .transpose()?;
                let action = match (evaluated.health, existing.mode, resolved) {
                    (DeploymentHealth::Clean, old, current_mode) if old == current_mode => {
                        PlanAction::LeaveUntouched
                    }
                    (DeploymentHealth::Clean, _, _) => PlanAction::Replace,
                    (DeploymentHealth::VaultAhead, _, DeploymentMode::ManagedCopy) => {
                        PlanAction::Replace
                    }
                    (
                        DeploymentHealth::VaultAhead,
                        DeploymentMode::Symlink,
                        DeploymentMode::Symlink,
                    ) => PlanAction::LeaveUntouched,
                    (health, _, _) => {
                        return Err(DeploymentError::DriftBlocked(format!(
                            "{health:?} cannot be overwritten by redeploy"
                        )));
                    }
                };
                (
                    existing.id,
                    action,
                    current_fingerprint(&target, &existing, &evaluated, now)?,
                    evaluated.health,
                    Some(existing.expected_digest),
                    previous_expected_link_target,
                    existing.created_at,
                    existing.updated_at,
                )
            }
        };
        let after = desired_fingerprint(
            &target,
            skill_id,
            deployment_id,
            resolved,
            current.digest,
            &working,
            now,
        )?;
        let after = if action == PlanAction::LeaveUntouched {
            before.clone()
        } else {
            after
        };
        let snapshot = (action == PlanAction::Replace).then(SnapshotId::generate);
        let context = deployment_context(
            DeploymentProductAction::Deploy,
            &skill,
            &target,
            project.as_ref(),
            capability,
            deployment_id,
            existing_deployment,
            requested,
            resolved,
            fallback_reason,
            previous_digest,
            previous_link,
            health,
            None,
            created_at,
            updated_at,
            current.digest,
            snapshot,
            self.vault.paths.root(),
        )?;
        let step = PlanStep::new(
            action,
            PlanPath::from_authorized(target_id, &authorized)
                .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?,
            Some(requested),
            Some(resolved),
            before,
            after,
            action == PlanAction::Replace,
        );
        Ok((context, step, current.stats, now))
    }

    #[allow(clippy::too_many_lines)]
    pub fn plan_batch_deployment(
        &self,
        request: &BatchDeploymentPlanRequest,
    ) -> Result<BatchDeploymentPlanView, DeploymentError> {
        if !(2..=20).contains(&request.targets.len()) {
            return Err(DeploymentError::DriftBlocked(
                "a batch deployment requires between 2 and 20 targets".into(),
            ));
        }
        let skill_id = parse_id::<SkillId>(&request.skill_id, "Skill")?;
        let mut choices = request.targets.clone();
        choices.sort_by(|a, b| a.target_id.cmp(&b.target_id));
        if choices
            .windows(2)
            .any(|pair| pair[0].target_id == pair[1].target_id)
        {
            return Err(DeploymentError::DriftBlocked(
                "batch targets must be unique".into(),
            ));
        }
        let mut entries = Vec::with_capacity(choices.len());
        let mut steps = Vec::with_capacity(choices.len());
        let mut stats = None;
        let mut now = None;
        for (order, choice) in choices.iter().enumerate() {
            let (context, mut step, observed, created) =
                self.build_deployment(&DeploymentPlanRequest {
                    skill_id: request.skill_id.clone(),
                    target_id: choice.target_id.clone(),
                    requested_mode: choice.requested_mode,
                })?;
            step.order = u32::try_from(order)
                .map_err(|_| DeploymentError::DriftBlocked("too many targets".into()))?;
            let mut deployment = context.deployment;
            deployment.step_order = step.order;
            entries.push(BatchDeploymentEntryEvidence {
                target: context.target,
                deployment,
                inverse: None,
            });
            steps.push(step);
            stats = Some(observed);
            now = Some(created);
        }
        let now = now.expect("non-empty validated batch");
        let destructive = steps.iter().any(PlanStep::is_destructive);
        let snapshot_id = destructive.then(SnapshotId::generate);
        let activity_id = ActivityId::generate();
        let batch_context = BatchDeploymentPlanContext {
            action: BatchDeploymentAction::Deploy,
            skill: self
                .build_deployment(&DeploymentPlanRequest {
                    skill_id: request.skill_id.clone(),
                    target_id: choices[0].target_id.clone(),
                    requested_mode: choices[0].requested_mode,
                })?
                .0
                .skill,
            entries,
            activity_id,
            snapshot_id,
            undo_of: None,
        };
        let operation_id = OperationId::generate();
        let selected_target_ids = batch_context
            .entries
            .iter()
            .map(|e| e.target.target_id)
            .collect::<Vec<_>>();
        let selected_deployment_ids = batch_context
            .entries
            .iter()
            .map(|e| e.deployment.deployment_id)
            .collect::<Vec<_>>();
        let intent = OperationIntent {
            operation_id,
            kind: OperationKind::Deploy,
            selected_skill_ids: vec![skill_id],
            selected_target_ids: selected_target_ids.clone(),
            selected_deployment_ids: selected_deployment_ids.clone(),
            ownership_choices: Vec::new(),
        };
        let observed = stats.expect("non-empty validated batch");
        let plan_content = OperationPlanContent::new(
            operation_id,
            OperationKind::Deploy,
            now,
            now.checked_add(PLAN_TTL)
                .map_err(|e| DeploymentError::Journal(e.to_string()))?,
            vec![skill_id],
            selected_target_ids,
            selected_deployment_ids,
            Vec::new(),
            BundleCaps::default(),
            observed,
            steps,
            Vec::new(),
            RecoverySummary {
                snapshot_count: u32::from(destructive),
                estimated_staging_bytes: observed.regular_file_bytes * request.targets.len() as u64,
                estimated_snapshot_bytes: if destructive {
                    observed.regular_file_bytes
                } else {
                    0
                },
                estimated_rollback_bytes: if destructive {
                    observed.regular_file_bytes
                } else {
                    0
                },
                spans_filesystems: request.targets.len() > 1,
            },
            Vec::new(),
        )
        .with_batch_deployment_context(batch_context);
        let plan = OperationPlanner::new(
            OperationStore::open(self.vault.paths.manager())
                .map_err(|e| DeploymentError::Journal(e.to_string()))?,
        )
        .plan(
            &intent,
            &StaticDeploymentBuilder(plan_content),
            &CancellationToken::default(),
        )?;
        batch_deployment_plan_view(&plan)
    }

    /// Plans the inverse of a successful batch deployment after proving every postcondition.
    #[allow(clippy::too_many_lines)]
    pub fn plan_undo(
        &self,
        operation_id: &str,
    ) -> Result<BatchDeploymentPlanView, DeploymentError> {
        let original_id = parse_id::<OperationId>(operation_id, "Operation")?;
        let store = OperationStore::open(self.vault.paths.manager())
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let stored = store
            .load(original_id)
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        if stored.journal.state != OperationState::Finalized
            || stored.journal.outcome != Some(OperationOutcome::Succeeded)
        {
            return Err(DeploymentError::DriftBlocked(
                "only a successful finalized batch deployment can be undone".into(),
            ));
        }
        let original = stored
            .plan
            .content
            .batch_deployment
            .as_ref()
            .filter(|context| context.action == BatchDeploymentAction::Deploy)
            .ok_or_else(|| {
                DeploymentError::DriftBlocked("operation is not a batch deployment".into())
            })?;
        let snapshot = read_deployment_snapshot_for_planning(&store, original_id).ok();
        // Validate all sealed filesystem and durable source evidence before creating an inverse
        // journal. Database health is deliberately not used as a substitute for the plan's exact
        // postcondition.
        for (entry, step) in original.entries.iter().zip(&stored.plan.content.steps) {
            verify_sealed_postcondition(step, stored.plan.content.bundle_caps)?;
            if step.action == PlanAction::Replace {
                let protection = stored
                    .journal
                    .snapshot_protections
                    .iter()
                    .find(|protection| protection.step_order == step.order)
                    .ok_or_else(|| {
                        DeploymentError::DriftBlocked(
                            "original Replace has no finalized SnapshotProtection".into(),
                        )
                    })?;
                if protection.before != step.before
                    || snapshot.as_ref().and_then(|evidence| {
                        evidence
                            .protections
                            .iter()
                            .find(|candidate| candidate.step_order == step.order)
                    }) != Some(protection)
                {
                    return Err(DeploymentError::DriftBlocked(
                        "original Replace Snapshot evidence changed".into(),
                    ));
                }
                verify_protected_reference(
                    &self.vault,
                    protection,
                    stored.plan.content.bundle_caps,
                )?;
            }
            let deployment = self
                .vault
                .repositories
                .deployment(entry.deployment.deployment_id)?
                .filter(|deployment| {
                    deployment.active && deployment.last_operation_id == Some(original_id)
                })
                .ok_or_else(|| {
                    DeploymentError::DriftBlocked(
                        "batch deployment projection changed after execution".into(),
                    )
                })?;
            if self.evaluate(&deployment, false)?.health != DeploymentHealth::Clean {
                return Err(DeploymentError::DriftBlocked(
                    "a batch target changed after deployment".into(),
                ));
            }
        }
        let now = UtcTimestamp::now();
        let new_id = OperationId::generate();
        let snapshot_id = SnapshotId::generate();
        let mut entries = Vec::with_capacity(original.entries.len());
        let mut steps = Vec::with_capacity(original.entries.len());
        for (order, (entry, step)) in original
            .entries
            .iter()
            .zip(&stored.plan.content.steps)
            .rev()
            .enumerate()
        {
            let mut deployment = entry.deployment.clone();
            let step_order = u32::try_from(order)
                .map_err(|_| DeploymentError::DriftBlocked("too many inverse steps".into()))?;
            deployment.step_order = step_order;
            let mut inverse = step.inverse();
            inverse.order = step_order;
            inverse.recovery_required = inverse.is_destructive();
            if inverse.action == PlanAction::Replace {
                let restored_mode = if inverse.after.expected_kind == EntryKind::Symlink {
                    DeploymentMode::Symlink
                } else {
                    DeploymentMode::ManagedCopy
                };
                deployment.requested_mode = restored_mode;
                deployment.resolved_mode = restored_mode;
                deployment.fallback_reason = None;
                inverse.requested_mode = Some(restored_mode);
                inverse.resolved_mode = Some(restored_mode);
            }
            inverse.before.metadata = Some(MetadataFingerprint::from_metadata(
                &fs::symlink_metadata(Path::new(step.path.display_path()))?,
            ));
            inverse.after.metadata = None;
            let protected_reference = if step.action == PlanAction::Replace {
                Some(
                    stored
                        .journal
                        .snapshot_protections
                        .iter()
                        .find(|protection| protection.step_order == step.order)
                        .expect("validated protection")
                        .reference
                        .clone(),
                )
            } else {
                None
            };
            entries.push(BatchDeploymentEntryEvidence {
                target: entry.target.clone(),
                deployment,
                inverse: Some(BatchDeploymentInverseEvidence {
                    source_operation_id: original_id,
                    source_step_order: step.order,
                    protected_reference,
                }),
            });
            steps.push(inverse);
        }
        let target_ids = entries
            .iter()
            .map(|entry| entry.target.target_id)
            .collect::<Vec<_>>();
        let deployment_ids = entries
            .iter()
            .map(|entry| entry.deployment.deployment_id)
            .collect::<Vec<_>>();
        let batch_context = BatchDeploymentPlanContext {
            action: BatchDeploymentAction::Undo,
            skill: original.skill.clone(),
            entries,
            activity_id: ActivityId::generate(),
            snapshot_id: Some(snapshot_id),
            undo_of: Some(original_id),
        };
        let intent = OperationIntent {
            operation_id: new_id,
            kind: OperationKind::Undo,
            selected_skill_ids: vec![original.skill.skill_id],
            selected_target_ids: target_ids.clone(),
            selected_deployment_ids: deployment_ids.clone(),
            ownership_choices: Vec::new(),
        };
        let plan_content = OperationPlanContent::new(
            new_id,
            OperationKind::Undo,
            now,
            now.checked_add(PLAN_TTL)
                .map_err(|error| DeploymentError::Journal(error.to_string()))?,
            vec![original.skill.skill_id],
            target_ids,
            deployment_ids,
            Vec::new(),
            stored.plan.content.bundle_caps,
            stored.plan.content.observed_bundle_stats,
            steps,
            Vec::new(),
            RecoverySummary {
                snapshot_count: 1,
                estimated_staging_bytes: 0,
                estimated_snapshot_bytes: stored
                    .plan
                    .content
                    .observed_bundle_stats
                    .regular_file_bytes
                    * original.entries.len() as u64,
                estimated_rollback_bytes: stored
                    .plan
                    .content
                    .observed_bundle_stats
                    .regular_file_bytes
                    * original.entries.len() as u64,
                spans_filesystems: true,
            },
            Vec::new(),
        )
        .with_batch_deployment_context(batch_context);
        let plan = OperationPlanner::new(store).plan(
            &intent,
            &StaticDeploymentBuilder(plan_content),
            &CancellationToken::default(),
        )?;
        batch_deployment_plan_view(&plan)
    }

    #[allow(clippy::too_many_lines)]
    pub fn plan_undeploy(
        &self,
        request: &UndeployPlanRequest,
    ) -> Result<DeploymentPlanView, DeploymentError> {
        if request.resolution == UndeployResolutionDto::Cancel {
            return Err(DeploymentError::PlanningCancelled);
        }
        let deployment_id = parse_id::<DeploymentId>(&request.deployment_id, "Deployment")?;
        let deployment = self
            .vault
            .repositories
            .deployment(deployment_id)?
            .filter(|deployment| deployment.active)
            .ok_or(DeploymentError::DeploymentMissing)?;
        let skill = self
            .vault
            .repositories
            .skill(deployment.skill_id)?
            .filter(|skill| skill.lifecycle == SkillLifecycle::Active)
            .ok_or(DeploymentError::SkillMissing)?;
        let (target, project, root) = self.open_target(deployment.target_id)?;
        let relative = BundleRelativePath::parse(deployment.deployment_name.as_str())
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?;
        let authorized = root
            .authorize(&relative)
            .map_err(|_| DeploymentError::TargetMissing)?;
        if authorized.path() != deployment.target_path {
            return Err(DeploymentError::TargetMissing);
        }
        let evaluated = self.evaluate(&deployment, false)?;
        let resolution = match request.resolution {
            UndeployResolutionDto::RemoveManaged if evaluated.health == DeploymentHealth::Clean => {
                UndeployResolution::RemoveManaged
            }
            UndeployResolutionDto::RemoveManaged => {
                return Err(DeploymentError::DriftBlocked(format!(
                    "{:?} target cannot be deleted silently",
                    evaluated.health
                )));
            }
            UndeployResolutionDto::PreserveTarget
                if evaluated.health != DeploymentHealth::Clean =>
            {
                UndeployResolution::PreserveTarget
            }
            UndeployResolutionDto::PreserveTarget => {
                return Err(DeploymentError::DriftBlocked(
                    "a clean managed target should be removed rather than abandoned".to_owned(),
                ));
            }
            UndeployResolutionDto::Cancel => unreachable!("handled above"),
        };
        let now = UtcTimestamp::now();
        let before = current_fingerprint(&target, &deployment, &evaluated, now)?;
        let (action, after, snapshot) = match resolution {
            UndeployResolution::RemoveManaged => (
                PlanAction::Remove,
                fingerprint(
                    &target,
                    EntryKind::Absent,
                    None,
                    None,
                    None,
                    None,
                    Some(deployment.skill_id),
                    Some(deployment.id),
                    now,
                ),
                Some(SnapshotId::generate()),
            ),
            UndeployResolution::PreserveTarget => {
                (PlanAction::LeaveUntouched, before.clone(), None)
            }
        };
        let working = self.vault.paths.root().join(skill.working_path.as_str());
        let current = hash_bundle(&working, BundleCaps::default())
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?;
        let capability = self
            .capability_probe
            .inspect(root.canonical_path(), &working)?;
        if resolution == UndeployResolution::RemoveManaged {
            require_base_capability(&capability)?;
        }
        let previous_expected_link_target = deployment
            .expected_link_target
            .as_deref()
            .map(|path| exact_plan_path(path, "reviewed expected symlink target"))
            .transpose()?;
        let context = deployment_context(
            DeploymentProductAction::Undeploy,
            &skill,
            &target,
            project.as_ref(),
            capability,
            deployment.id,
            true,
            deployment.mode,
            deployment.mode,
            None,
            Some(deployment.expected_digest),
            previous_expected_link_target,
            evaluated.health,
            Some(resolution),
            deployment.created_at,
            deployment.updated_at,
            current.digest,
            snapshot,
            self.vault.paths.root(),
        )?;
        let step = PlanStep::new(
            action,
            PlanPath::from_authorized(target.id, &authorized)
                .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?,
            Some(deployment.mode),
            Some(deployment.mode),
            before,
            after,
            action == PlanAction::Remove,
        );
        self.persist_plan(context, step, current.stats, now)
    }

    fn persist_plan(
        &self,
        context: DeploymentPlanContext,
        step: PlanStep,
        stats: BundleStats,
        now: UtcTimestamp,
    ) -> Result<DeploymentPlanView, DeploymentError> {
        let kind = match context.action {
            DeploymentProductAction::Deploy => OperationKind::Deploy,
            DeploymentProductAction::Undeploy => OperationKind::Undeploy,
        };
        let operation_id = OperationId::generate();
        let intent = OperationIntent {
            operation_id,
            kind,
            selected_skill_ids: vec![context.skill.skill_id],
            selected_target_ids: vec![context.target.target_id],
            selected_deployment_ids: vec![context.deployment.deployment_id],
            ownership_choices: Vec::new(),
        };
        let destructive = step.is_destructive();
        let plan_content = OperationPlanContent::new(
            operation_id,
            kind,
            now,
            now.checked_add(PLAN_TTL)
                .map_err(|error| DeploymentError::Journal(error.to_string()))?,
            intent.selected_skill_ids.clone(),
            intent.selected_target_ids.clone(),
            intent.selected_deployment_ids.clone(),
            Vec::new(),
            BundleCaps::default(),
            stats,
            vec![step],
            Vec::new(),
            RecoverySummary {
                snapshot_count: u32::from(destructive),
                estimated_staging_bytes: stats.regular_file_bytes,
                estimated_snapshot_bytes: if destructive {
                    stats.regular_file_bytes
                } else {
                    0
                },
                estimated_rollback_bytes: if destructive {
                    stats.regular_file_bytes
                } else {
                    0
                },
                spans_filesystems: false,
            },
            Vec::new(),
        )
        .with_deployment_context(context);
        let store = OperationStore::open(self.vault.paths.manager())
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let plan = OperationPlanner::new(store).plan(
            &intent,
            &StaticDeploymentBuilder(plan_content),
            &CancellationToken::default(),
        )?;
        deployment_plan_view(&plan)
    }

    pub fn execute_any_operation(
        &self,
        operation_id: &str,
        plan_digest: &str,
    ) -> Result<AnyOperationView, DeploymentError> {
        let id = parse_id::<OperationId>(operation_id, "Operation")?;
        let store = OperationStore::open(self.vault.paths.manager())
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let stored = store
            .load(id)
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        if stored.plan.plan_digest.to_string() != plan_digest {
            return Err(DeploymentError::DriftBlocked(
                "plan digest differs from reviewed plan".to_owned(),
            ));
        }
        let (vault_root, targets) = if let Some(context) = &stored.plan.content.deployment {
            (&context.skill.vault_root, vec![&context.target])
        } else if let Some(context) = &stored.plan.content.batch_deployment {
            (
                &context.skill.vault_root,
                context.entries.iter().map(|entry| &entry.target).collect(),
            )
        } else {
            return Err(DeploymentError::Journal(
                "Operation is not deployment-owned".into(),
            ));
        };
        if Path::new(vault_root) != self.vault.paths.root() {
            return Err(DeploymentError::TargetMissing);
        }
        if stored.journal.state == OperationState::Planned {
            for target in &targets {
                let current = self
                    .vault
                    .repositories
                    .target(target.target_id)?
                    .ok_or(DeploymentError::TargetMissing)?;
                ensure_target_is_configured(&self.vault.repositories, &current)?;
            }
        }
        let mut roots = TargetRoots::new();
        for target in targets {
            let target_root = AuthorizedRoot::open(&target.target_root)
                .map_err(|_| DeploymentError::TargetMissing)?;
            if target_root.canonical_path() != Path::new(&target.target_canonical_root) {
                return Err(DeploymentError::TargetMissing);
            }
            roots.insert(target.target_id, target_root);
        }
        let token = CancellationToken::default();
        self.cancellations
            .lock()
            .map_err(|_| DeploymentError::Journal("cancellation registry is poisoned".into()))?
            .insert(id, token.clone());
        let hooks = Arc::new(DeploymentHooks {
            vault: Arc::clone(&self.vault),
            store: store.clone(),
            capability_probe: Arc::clone(&self.capability_probe),
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
            .map_err(|_| DeploymentError::Journal("cancellation registry is poisoned".into()))?
            .remove(&id);
        if result.is_err() {
            let _ = ActivityService::new(
                self.vault.repositories.clone(),
                OperationStore::open(self.vault.paths.manager())
                    .map_err(|error| DeploymentError::Journal(error.to_string()))?,
            )
            .project_terminal_operation(id);
        }
        let execution = result?;
        let mut view = self.operation_view(id)?;
        match &mut view {
            AnyOperationView::Deployment(value) => value.replayed = execution.replayed,
            AnyOperationView::BatchDeployment(value) => value.replayed = execution.replayed,
            AnyOperationView::Takeover(_) => unreachable!("deployment-owned operation"),
        }
        Ok(view)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn execute_operation(
        &self,
        operation_id: &str,
        plan_digest: &str,
    ) -> Result<DeploymentOperationView, DeploymentError> {
        match self.execute_any_operation(operation_id, plan_digest)? {
            AnyOperationView::Deployment(view) => Ok(view),
            _ => Err(DeploymentError::Journal(
                "Operation is not a single deployment".into(),
            )),
        }
    }

    pub fn operation_kind(&self, operation_id: &str) -> Result<OperationKind, DeploymentError> {
        let id = parse_id::<OperationId>(operation_id, "Operation")?;
        OperationStore::open(self.vault.paths.manager())
            .and_then(|store| store.load(id))
            .map(|stored| stored.plan.content.kind)
            .map_err(|error| DeploymentError::Journal(error.to_string()))
    }

    /// Returns a stable, human-readable rendering of the exact persisted Operation Plan.
    ///
    /// # Errors
    ///
    /// Returns an error when the Operation ID is invalid or its durable plan cannot be validated.
    pub fn export_plan_json(
        &self,
        operation_id: &str,
    ) -> Result<(String, String), DeploymentError> {
        let id = parse_id::<OperationId>(operation_id, "Operation")?;
        let stored = OperationStore::open(self.vault.paths.manager())
            .and_then(|store| store.load(id))
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let json = serde_json::to_string_pretty(&stored.plan)
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        Ok((stored.plan.plan_digest.to_string(), json))
    }

    /// Recovers one deployment-owned operation using only its persisted authority evidence.
    pub fn recover_operation(
        &self,
        id: OperationId,
    ) -> Result<crate::operations::OperationExecution, DeploymentError> {
        let store = OperationStore::open(self.vault.paths.manager())
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let stored = store
            .load(id)
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let (vault_root, targets) = if let Some(context) = &stored.plan.content.deployment {
            (&context.skill.vault_root, vec![&context.target])
        } else if let Some(context) = &stored.plan.content.batch_deployment {
            (
                &context.skill.vault_root,
                context.entries.iter().map(|entry| &entry.target).collect(),
            )
        } else {
            return Err(DeploymentError::Journal(
                "Operation is not deployment-owned".into(),
            ));
        };
        if Path::new(vault_root) != self.vault.paths.root() {
            return Err(DeploymentError::TargetMissing);
        }
        let mut roots = TargetRoots::new();
        for target in targets {
            let root = AuthorizedRoot::open(&target.target_root)
                .map_err(|_| DeploymentError::TargetMissing)?;
            if root.canonical_path() != Path::new(&target.target_canonical_root) {
                return Err(DeploymentError::TargetMissing);
            }
            roots.insert(target.target_id, root);
        }
        let hooks = Arc::new(DeploymentHooks {
            vault: Arc::clone(&self.vault),
            store: store.clone(),
            capability_probe: Arc::clone(&self.capability_probe),
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

    pub fn operation_view(&self, id: OperationId) -> Result<AnyOperationView, DeploymentError> {
        let stored = OperationStore::open(self.vault.paths.manager())
            .and_then(|store| store.load(id))
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let operation_id = id.to_string();
        let plan_digest = stored.plan.plan_digest.to_string();
        let state = stored.journal.state;
        let outcome = stored.journal.outcome;
        let terminal = state.is_terminal();
        let cancellation_allowed = !terminal;
        let tone = OperationTone::from_state(state, outcome);
        let failure = stored
            .journal
            .failure
            .as_ref()
            .map(|value| value.summary.clone());
        let recovery = stored
            .journal
            .snapshot_protections
            .iter()
            .map(|protection| format!("step {}: {}", protection.step_order, protection.reference))
            .collect::<Vec<_>>();
        if stored.plan.content.batch_deployment.is_some() {
            return Ok(AnyOperationView::BatchDeployment(
                BatchDeploymentOperationView {
                    operation_id,
                    plan_digest,
                    state: format!("{state:?}"),
                    outcome: outcome.map(|value| format!("{value:?}")),
                    terminal,
                    cancellation_allowed,
                    tone,
                    failure,
                    recovery,
                    review: batch_deployment_plan_view(&stored.plan)?,
                    replayed: false,
                },
            ));
        }
        Ok(AnyOperationView::Deployment(DeploymentOperationView {
            operation_id,
            plan_digest,
            state: format!("{state:?}"),
            outcome: outcome.map(|value| format!("{value:?}")),
            terminal,
            cancellation_allowed,
            tone,
            failure,
            recovery,
            review: deployment_plan_view(&stored.plan)?,
            replayed: false,
        }))
    }

    pub fn get_any_operation(
        &self,
        operation_id: &str,
    ) -> Result<AnyOperationView, DeploymentError> {
        self.operation_view(parse_id::<OperationId>(operation_id, "Operation")?)
    }

    #[allow(dead_code)] // Retained for the frozen single-target application contract and tests.
    pub fn get_operation(
        &self,
        operation_id: &str,
    ) -> Result<DeploymentOperationView, DeploymentError> {
        match self.get_any_operation(operation_id)? {
            AnyOperationView::Deployment(view) => Ok(view),
            _ => Err(DeploymentError::Journal(
                "Operation is not a single deployment".into(),
            )),
        }
    }

    pub fn cancel(&self, operation_id: &str) -> Result<bool, DeploymentError> {
        let id = parse_id::<OperationId>(operation_id, "Operation")?;
        Ok(self
            .cancellations
            .lock()
            .map_err(|_| DeploymentError::Journal("cancellation registry is poisoned".into()))?
            .get(&id)
            .is_some_and(|token| {
                token.cancel();
                true
            }))
    }

    pub fn verify(&self, deployment_id: &str) -> Result<DeploymentHealthView, DeploymentError> {
        let id = parse_id::<DeploymentId>(deployment_id, "Deployment")?;
        let deployment = self
            .vault
            .repositories
            .deployment(id)?
            .ok_or(DeploymentError::DeploymentMissing)?;
        self.evaluate(&deployment, true)
            .map(|evaluation| health_view(&deployment, evaluation))
    }

    pub fn deployments_list(
        &self,
        query: &DeploymentQuery,
    ) -> Result<DeploymentPage, DeploymentError> {
        if query.limit == 0 || usize::from(query.limit) > MAX_DEPLOYMENTS {
            return Err(DeploymentError::DriftBlocked(
                "deployment limit must be between 1 and 500".to_owned(),
            ));
        }
        let skill = query
            .skill_id
            .as_deref()
            .map(|value| parse_id::<SkillId>(value, "Skill"))
            .transpose()?;
        let target = query
            .target_id
            .as_deref()
            .map(|value| parse_id::<TargetId>(value, "Target"))
            .transpose()?;
        let records = self.vault.repositories.deployments(
            skill,
            target,
            query.include_inactive,
            usize::from(query.limit),
        )?;
        let items = records
            .iter()
            .map(|record| {
                self.evaluate(record, false)
                    .map(|evaluation| health_view(record, evaluation))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DeploymentPage {
            count: u16::try_from(items.len()).unwrap_or(u16::MAX),
            items,
        })
    }

    fn open_target(
        &self,
        id: TargetId,
    ) -> Result<(TargetRecord, Option<ProjectRecord>, AuthorizedRoot), DeploymentError> {
        let target = self
            .vault
            .repositories
            .target(id)?
            .ok_or(DeploymentError::TargetMissing)?;
        if !target.is_custom && !crate::adapters::is_known(&target.adapter_id) {
            return Err(DeploymentError::TargetMissing);
        }
        let project = target
            .project_id
            .map(|id| self.vault.repositories.project(id))
            .transpose()?
            .flatten();
        if (target.scope == "global" && project.is_some())
            || (target.scope == "project" && project.is_none() && !target.is_custom)
            || !matches!(target.scope.as_str(), "global" | "project")
        {
            return Err(DeploymentError::TargetMissing);
        }
        let root =
            AuthorizedRoot::open(&target.root_path).map_err(|_| DeploymentError::TargetMissing)?;
        if root.canonical_path() != target.canonical_root_path {
            return Err(DeploymentError::TargetMissing);
        }
        if let Some(metadata) = self
            .vault
            .repositories
            .target_registration_metadata(target.id)?
        {
            let identity = root.identity();
            if identity.device_id != metadata.root_device_id
                || identity.file_id != metadata.root_file_id
            {
                return Err(DeploymentError::TargetMissing);
            }
        }
        ensure_disjoint(self.vault.paths.root(), root.canonical_path())?;
        Ok((target, project, root))
    }

    fn evaluate(
        &self,
        deployment: &DeploymentRecord,
        persist: bool,
    ) -> Result<HealthEvaluation, DeploymentError> {
        let skill = self
            .vault
            .repositories
            .skill(deployment.skill_id)?
            .ok_or(DeploymentError::SkillMissing)?;
        let target = self
            .vault
            .repositories
            .target(deployment.target_id)?
            .ok_or(DeploymentError::TargetMissing)?;
        if deployment.adapter_version != target.adapter_id
            || target
                .canonical_root_path
                .join(deployment.deployment_name.as_str())
                != deployment.target_path
        {
            return Ok(HealthEvaluation::unverified(
                "Persisted Target authority differs from deployment evidence",
            ));
        }
        let manifest = match self.vault.manifests.read_deployment(deployment.id) {
            Ok(manifest) if manifest_matches(&manifest, deployment) => Some(manifest),
            Ok(_) => {
                return Ok(HealthEvaluation::conflict(
                    "Deployment manifest disagrees with the indexed relationship",
                ));
            }
            Err(ManifestError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => {
                return Ok(HealthEvaluation::unverified(
                    "Deployment manifest is unreadable or invalid",
                ));
            }
        };
        if deployment.active && manifest.is_none() {
            return Ok(HealthEvaluation::unverified(
                "Active deployment manifest is missing",
            ));
        }
        let vault = hash_bundle(
            &self.vault.paths.root().join(skill.working_path.as_str()),
            BundleCaps::default(),
        )
        .ok()
        .map(|hashed| hashed.digest);
        let evaluation = evaluate_target(deployment, vault);
        if persist && deployment.active {
            self.vault.repositories.update_deployment_health(
                deployment.id,
                evaluation.health,
                evaluation.verified_at,
            )?;
        }
        Ok(evaluation)
    }
}

struct StaticDeploymentBuilder(OperationPlanContent);

impl PlanBuilder for StaticDeploymentBuilder {
    fn build_content(
        &self,
        _intent: &OperationIntent,
        cancellation: &CancellationToken,
    ) -> Result<OperationPlanContent, OperationError> {
        cancellation.check()?;
        Ok(self.0.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeploymentSnapshotEvidence {
    schema_version: u32,
    operation_id: OperationId,
    snapshot_id: SnapshotId,
    protections: Vec<SnapshotProtection>,
}

struct DeploymentHooks {
    vault: Arc<OpenVault>,
    store: OperationStore,
    capability_probe: Arc<dyn CapabilityProbe>,
}

impl DeploymentHooks {
    fn verify_inverse_source<'a>(
        &self,
        plan: &'a OperationPlan,
        step: &PlanStep,
    ) -> Result<&'a BatchDeploymentEntryEvidence, OperationHookError> {
        let batch = plan
            .content
            .batch_deployment
            .as_ref()
            .ok_or_else(|| hook("missing batch context"))?;
        let entry = batch
            .entries
            .iter()
            .find(|entry| entry.deployment.step_order == step.order)
            .ok_or_else(|| hook("missing inverse entry"))?;
        let inverse = entry
            .inverse
            .as_ref()
            .ok_or_else(|| hook("missing inverse evidence"))?;
        let source = self
            .store
            .load(inverse.source_operation_id)
            .map_err(|error| hook(error.to_string()))?;
        let source_step = source
            .plan
            .content
            .steps
            .iter()
            .find(|candidate| candidate.order == inverse.source_step_order)
            .ok_or_else(|| hook("source step changed"))?;
        if source.journal.state != OperationState::Finalized
            || source.journal.outcome != Some(OperationOutcome::Succeeded)
            || {
                let mut reviewed_before = step.before.clone();
                reviewed_before.metadata = source_step.after.metadata;
                reviewed_before != source_step.after
            }
            || {
                let mut reviewed_after = step.after.clone();
                reviewed_after.metadata = source_step.before.metadata;
                reviewed_after != source_step.before
            }
            || step.action != source_step.action.inverse()
        {
            return Err(hook("sealed source operation evidence changed"));
        }
        if source_step.action == PlanAction::Replace {
            let protection = source
                .journal
                .snapshot_protections
                .iter()
                .find(|candidate| candidate.step_order == source_step.order)
                .ok_or_else(|| hook("source SnapshotProtection is missing"))?;
            let snapshot = read_deployment_snapshot(&self.store, inverse.source_operation_id)?;
            if protection.before != source_step.before
                || inverse.protected_reference.as_deref() != Some(&protection.reference)
                || snapshot
                    .protections
                    .iter()
                    .find(|candidate| candidate.step_order == source_step.order)
                    != Some(protection)
            {
                return Err(hook("sealed protected reference changed"));
            }
            verify_protected_reference(&self.vault, protection, plan.content.bundle_caps)
                .map_err(|error| hook(error.to_string()))?;
        } else if inverse.protected_reference.is_some() {
            return Err(hook("Create inverse unexpectedly has protected bytes"));
        }
        Ok(entry)
    }

    fn context(plan: &OperationPlan) -> Result<&DeploymentPlanContext, OperationHookError> {
        plan.content
            .deployment
            .as_ref()
            .ok_or_else(|| hook("missing deployment context"))
    }

    fn single_plan(
        plan: &OperationPlan,
        step: &PlanStep,
    ) -> Result<OperationPlan, OperationHookError> {
        let batch = plan
            .content
            .batch_deployment
            .as_ref()
            .ok_or_else(|| hook("missing batch deployment context"))?;
        let entry = batch
            .entries
            .iter()
            .find(|entry| entry.deployment.step_order == step.order)
            .ok_or_else(|| hook("missing batch entry for step"))?;
        let mut single_step = step.clone();
        single_step.order = 0;
        let mut deployment = entry.deployment.clone();
        deployment.step_order = 0;
        let entry_authority = DeploymentPlanContext {
            action: if batch.action == BatchDeploymentAction::Deploy {
                DeploymentProductAction::Deploy
            } else {
                DeploymentProductAction::Undeploy
            },
            skill: batch.skill.clone(),
            target: entry.target.clone(),
            deployment,
            activity_id: batch.activity_id,
            snapshot_id: step.is_destructive().then_some(batch.snapshot_id).flatten(),
        };
        let mut sealed = plan.content.clone();
        sealed.schema_version = 3;
        sealed.kind = if batch.action == BatchDeploymentAction::Deploy {
            OperationKind::Deploy
        } else {
            OperationKind::Undeploy
        };
        sealed.selected_target_ids = vec![entry.target.target_id];
        sealed.selected_deployment_ids = vec![entry.deployment.deployment_id];
        sealed.steps = vec![single_step];
        sealed.recovery.snapshot_count = u32::from(step.is_destructive());
        sealed.batch_deployment = None;
        sealed.deployment = Some(entry_authority);
        OperationPlan::build(sealed).map_err(|error| hook(error.to_string()))
    }

    #[allow(clippy::too_many_lines)]
    fn revalidate_relationship(
        &self,
        plan: &OperationPlan,
        context: &DeploymentPlanContext,
        final_path: &Path,
        working: &Path,
    ) -> Result<(), OperationHookError> {
        let current = self
            .vault
            .repositories
            .deployment(context.deployment.deployment_id)
            .map_err(|error| hook(error.to_string()))?;
        let finalizing = self
            .store
            .load(plan.content.operation_id)
            .map(|stored| {
                matches!(
                    stored.journal.state,
                    OperationState::Committed | OperationState::Finalized
                )
            })
            .map_err(|error| hook(error.to_string()))?;
        let previous_mode = if context.deployment.previous_expected_link_target.is_some() {
            DeploymentMode::Symlink
        } else {
            DeploymentMode::ManagedCopy
        };
        let reviewed_record = current.as_ref().is_some_and(|record| {
            record.id == context.deployment.deployment_id
                && record.skill_id == context.skill.skill_id
                && record.target_id == context.target.target_id
                && record.deployment_name == context.skill.deployment_name
                && record.target_path == final_path
                && record.mode == previous_mode
                && Some(record.expected_digest) == context.deployment.previous_expected_digest
                && record.expected_link_target.as_deref()
                    == context
                        .deployment
                        .previous_expected_link_target
                        .as_deref()
                        .map(Path::new)
                && record.adapter_version == context.target.adapter_id
                && record.active == context.deployment.active_before
                && same_persisted_time(record.created_at, context.deployment.deployment_created_at)
                && same_persisted_time(record.updated_at, context.deployment.deployment_updated_at)
        });
        let finalized_digest = match context.action {
            DeploymentProductAction::Deploy => context.skill.reviewed_digest,
            DeploymentProductAction::Undeploy => context
                .deployment
                .previous_expected_digest
                .ok_or_else(|| hook("undeploy evidence has no prior expected digest"))?,
        };
        let finalized_link = match (context.action, context.deployment.resolved_mode) {
            (DeploymentProductAction::Deploy, DeploymentMode::Symlink) => Some(working),
            (DeploymentProductAction::Deploy, DeploymentMode::ManagedCopy) => None,
            (DeploymentProductAction::Undeploy, _) => context
                .deployment
                .previous_expected_link_target
                .as_deref()
                .map(Path::new),
        };
        let finalized_record = current.as_ref().is_some_and(|record| {
            record.id == context.deployment.deployment_id
                && record.skill_id == context.skill.skill_id
                && record.target_id == context.target.target_id
                && record.deployment_name == context.skill.deployment_name
                && record.target_path == final_path
                && record.mode == context.deployment.resolved_mode
                && record.expected_digest == finalized_digest
                && record.expected_link_target.as_deref() == finalized_link
                && record.adapter_version == context.target.adapter_id
                && record.active == (context.action == DeploymentProductAction::Deploy)
                && record.last_operation_id == Some(plan.content.operation_id)
                && same_persisted_time(record.created_at, context.deployment.deployment_created_at)
        });
        if context.deployment.existing_deployment {
            if !(reviewed_record || finalizing && finalized_record) {
                return Err(hook("reviewed Deployment authority changed"));
            }
        } else {
            let collision = self
                .vault
                .repositories
                .active_deployment_for_target_name(
                    context.target.target_id,
                    context.skill.deployment_name.clone(),
                )
                .map_err(|error| hook(error.to_string()))?;
            if collision.is_some() && !(finalizing && finalized_record) {
                return Err(hook("another Deployment claimed the reviewed Target name"));
            }
            if current.is_some() && !(finalizing && finalized_record) {
                return Err(hook("generated Deployment identity is already occupied"));
            }
        }

        let manifest = match self
            .vault
            .manifests
            .read_deployment(context.deployment.deployment_id)
        {
            Ok(manifest) => Some(manifest),
            Err(ManifestError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(hook(error.to_string())),
        };
        let reviewed_manifest =
            current
                .as_ref()
                .zip(manifest.as_ref())
                .is_some_and(|(record, manifest)| {
                    reviewed_record && manifest_matches(manifest, record)
                });
        let finalized_manifest = manifest.as_ref().is_some_and(|manifest| {
            context.action == DeploymentProductAction::Deploy
                && manifest.deployment_id == context.deployment.deployment_id
                && manifest.skill_id == context.skill.skill_id
                && manifest.target_id == context.target.target_id
                && manifest.deployment_name == context.skill.deployment_name
                && manifest.mode == context.deployment.resolved_mode
                && manifest.target_path == final_path
                && manifest.expected_digest == context.skill.reviewed_digest
                && manifest.expected_link_target.as_deref() == finalized_link
                && manifest.adapter_version == context.target.adapter_id
                && manifest.last_finalized_operation_id == plan.content.operation_id
        }) || (context.action == DeploymentProductAction::Undeploy
            && manifest.is_none());
        if context.deployment.existing_deployment {
            if !(reviewed_manifest || finalizing && finalized_manifest) {
                return Err(hook("reviewed Deployment manifest changed"));
            }
        } else if manifest.is_some() && !(finalizing && finalized_manifest) {
            return Err(hook("generated Deployment manifest identity is occupied"));
        }
        Ok(())
    }

    fn revalidate(&self, plan: &OperationPlan) -> Result<PathBuf, OperationHookError> {
        let context = Self::context(plan)?;
        let skill = self
            .vault
            .repositories
            .skill(context.skill.skill_id)
            .map_err(|error| hook(error.to_string()))?
            .ok_or_else(|| hook("reviewed Skill no longer exists"))?;
        if skill.lifecycle != SkillLifecycle::Active
            || skill.deployment_name != context.skill.deployment_name
            || skill.working_path != context.skill.working_bundle_path
            || skill.working_digest != context.skill.reviewed_digest
        {
            return Err(hook("reviewed Skill authority changed"));
        }
        let working = self
            .vault
            .paths
            .root()
            .join(context.skill.working_bundle_path.as_str());
        if hash_bundle(&working, plan.content.bundle_caps)
            .map_err(|error| hook(error.to_string()))?
            .digest
            != context.skill.reviewed_digest
        {
            return Err(hook("reviewed Vault working digest changed"));
        }
        let target = self
            .vault
            .repositories
            .target(context.target.target_id)
            .map_err(|error| hook(error.to_string()))?
            .ok_or_else(|| hook("reviewed Target no longer exists"))?;
        if target.adapter_id != context.target.adapter_id
            || target.scope != target_scope_text(context.target.target_scope)
            || target.root_path != Path::new(&context.target.target_root)
            || target.canonical_root_path != Path::new(&context.target.target_canonical_root)
            || target.project_id != context.target.project_id
            || target.is_override != context.target.is_override
            || target.is_custom != context.target.is_custom
        {
            return Err(hook("reviewed Target authority changed"));
        }
        let classification = target
            .project_id
            .map(|id| self.vault.repositories.project(id))
            .transpose()
            .map_err(|error| hook(error.to_string()))?
            .flatten()
            .map(|project| project.git_classification);
        if classification != context.target.project_git_classification {
            return Err(hook("reviewed project authority changed"));
        }
        let root =
            AuthorizedRoot::open(&target.root_path).map_err(|error| hook(error.to_string()))?;
        if root.canonical_path() != Path::new(&context.target.target_canonical_root) {
            return Err(hook("reviewed Target root identity changed"));
        }
        ensure_disjoint(self.vault.paths.root(), root.canonical_path())
            .map_err(|error| hook(error.to_string()))?;
        let final_path = root
            .canonical_path()
            .join(context.deployment.target_relative_path.as_str());
        ensure_no_name_collision(
            root.canonical_path(),
            &context.skill.deployment_name,
            &final_path,
        )
        .map_err(|error| hook(error.to_string()))?;
        self.revalidate_relationship(plan, context, &final_path, &working)?;
        let capability = self
            .capability_probe
            .inspect(root.canonical_path(), &working)
            .map_err(|error| hook(error.to_string()))?;
        if capability != context.target.capability {
            return Err(hook(
                "reviewed Target capability changed; generate a new plan",
            ));
        }
        Ok(working)
    }
}

impl StagingProvider for DeploymentHooks {
    fn stage(
        &self,
        plan: &OperationPlan,
        step: &PlanStep,
        staging_path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), OperationHookError> {
        cancellation
            .check()
            .map_err(|error| hook(error.to_string()))?;
        if plan
            .content
            .batch_deployment
            .as_ref()
            .is_some_and(|batch| batch.action == BatchDeploymentAction::Undo)
        {
            if step.action != PlanAction::Replace {
                return Err(hook("inverse Remove does not stage a final entry"));
            }
            let entry = self.verify_inverse_source(plan, step)?;
            let reference = entry
                .inverse
                .as_ref()
                .and_then(|inverse| inverse.protected_reference.as_deref())
                .ok_or_else(|| hook("inverse Replace has no protected reference"))?;
            if let Some(encoded) = reference.strip_prefix("object:") {
                let digest =
                    BundleDigest::from_str(encoded).map_err(|error| hook(error.to_string()))?;
                copy_bundle_exact(
                    &self.vault.objects.object_path(digest).join("bundle"),
                    staging_path,
                    plan.content.bundle_caps,
                )
                .map_err(|error| hook(error.to_string()))?;
            } else if let Some(target) = reference.strip_prefix("link:") {
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, staging_path)
                    .map_err(|error| hook(error.to_string()))?;
            } else {
                return Err(hook("unsupported protected reference"));
            }
            return Ok(());
        }
        if plan.content.batch_deployment.is_some() {
            let single = Self::single_plan(plan, step)?;
            return self.stage(
                &single,
                &single.content.steps[0],
                staging_path,
                cancellation,
            );
        }
        let context = Self::context(plan)?;
        if context.action != DeploymentProductAction::Deploy {
            return Err(hook("undeploy does not stage a final entry"));
        }
        let working = self.revalidate(plan)?;
        cancellation
            .check()
            .map_err(|error| hook(error.to_string()))?;
        match context.deployment.resolved_mode {
            DeploymentMode::ManagedCopy => {
                let copied = copy_bundle_exact(&working, staging_path, plan.content.bundle_caps)
                    .map_err(|error| hook(error.to_string()))?;
                if copied.digest != context.skill.reviewed_digest {
                    return Err(hook("staged Managed Copy digest mismatch"));
                }
            }
            DeploymentMode::Symlink => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&working, staging_path)
                    .map_err(|error| hook(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn revalidate_before_commit(
        &self,
        plan: &OperationPlan,
        step: &PlanStep,
    ) -> Result<(), OperationHookError> {
        if plan
            .content
            .batch_deployment
            .as_ref()
            .is_some_and(|batch| batch.action == BatchDeploymentAction::Undo)
        {
            self.verify_inverse_source(plan, step).map(|_| ())
        } else if plan.content.batch_deployment.is_some() {
            let single = Self::single_plan(plan, step)?;
            self.revalidate(&single).map(|_| ())
        } else {
            self.revalidate(plan).map(|_| ())
        }
    }
}

impl SnapshotRegistrar for DeploymentHooks {
    fn register(
        &self,
        plan: &OperationPlan,
        protected_steps: &[PlanStep],
        cancellation: &CancellationToken,
    ) -> Result<SnapshotRegistration, OperationHookError> {
        if protected_steps.is_empty() {
            return Ok(SnapshotRegistration::default());
        }
        let snapshot_id = plan
            .content
            .deployment
            .as_ref()
            .and_then(|context| context.snapshot_id)
            .or_else(|| {
                plan.content
                    .batch_deployment
                    .as_ref()
                    .and_then(|context| context.snapshot_id)
            })
            .ok_or_else(|| hook("destructive deployment plan has no Snapshot ID"))?;
        let mut protections = Vec::new();
        for step in protected_steps {
            cancellation
                .check()
                .map_err(|error| hook(error.to_string()))?;
            let reference = match step.before.expected_kind {
                EntryKind::Directory => {
                    let digest = step
                        .before
                        .bundle_digest
                        .ok_or_else(|| hook("destructive directory has no reviewed digest"))?;
                    self.vault
                        .objects
                        .publish(
                            plan.content.operation_id,
                            Path::new(step.path.display_path()),
                            Some(digest),
                            plan.content.created_at,
                        )
                        .map_err(|error| hook(error.to_string()))?;
                    format!("object:{digest}")
                }
                EntryKind::Symlink => format!(
                    "link:{}",
                    step.before
                        .raw_symlink_target
                        .as_deref()
                        .ok_or_else(|| hook("destructive link has no raw target"))?
                ),
                _ => {
                    return Err(hook(
                        "destructive target has no exact Snapshot representation",
                    ));
                }
            };
            protections.push(SnapshotProtection {
                step_order: step.order,
                reference,
                before: step.before.clone(),
            });
        }
        let evidence = DeploymentSnapshotEvidence {
            schema_version: SNAPSHOT_SCHEMA,
            operation_id: plan.content.operation_id,
            snapshot_id,
            protections: protections.clone(),
        };
        let path = deployment_snapshot_path(&self.store, plan.content.operation_id);
        let bytes =
            serde_json::to_vec_pretty(&evidence).map_err(|error| hook(error.to_string()))?;
        if path.exists() {
            let existing: DeploymentSnapshotEvidence =
                serde_json::from_slice(&fs::read(&path).map_err(|error| hook(error.to_string()))?)
                    .map_err(|error| hook(error.to_string()))?;
            if existing != evidence {
                return Err(hook("durable deployment Snapshot evidence differs"));
            }
        } else {
            crate::filesystem::durable::atomic_write(&path, &bytes)
                .map_err(|error| hook(error.to_string()))?;
        }
        Ok(SnapshotRegistration { protections })
    }
}

impl OperationFinalizer for DeploymentHooks {
    fn publish_manifests(
        &self,
        plan: &OperationPlan,
        journal: &crate::operations::OperationJournal,
    ) -> Result<(), OperationHookError> {
        if let Some(batch) = plan
            .content
            .batch_deployment
            .as_ref()
            .filter(|batch| batch.action == BatchDeploymentAction::Undo)
        {
            for entry in &batch.entries {
                let inverse = entry
                    .inverse
                    .as_ref()
                    .ok_or_else(|| hook("missing inverse evidence"))?;
                let source = self
                    .store
                    .load(inverse.source_operation_id)
                    .map_err(|error| hook(error.to_string()))?;
                let source_step = source
                    .plan
                    .content
                    .steps
                    .iter()
                    .find(|step| step.order == inverse.source_step_order)
                    .ok_or_else(|| hook("source step missing"))?;
                if source_step.action == PlanAction::Create {
                    self.vault
                        .manifests
                        .remove_deployment(entry.deployment.deployment_id)
                        .map_err(|error| hook(error.to_string()))?;
                } else {
                    let expected_digest = entry
                        .deployment
                        .previous_expected_digest
                        .ok_or_else(|| hook("replaced deployment has no prior digest"))?;
                    self.vault
                        .manifests
                        .write_deployment(&DeploymentManifest {
                            schema_version: 1,
                            deployment_id: entry.deployment.deployment_id,
                            skill_id: batch.skill.skill_id,
                            target_id: entry.target.target_id,
                            deployment_name: batch.skill.deployment_name.clone(),
                            mode: if entry.deployment.previous_expected_link_target.is_some() {
                                DeploymentMode::Symlink
                            } else {
                                DeploymentMode::ManagedCopy
                            },
                            target_path: Path::new(&entry.target.target_canonical_root)
                                .join(entry.deployment.target_relative_path.as_str()),
                            expected_digest,
                            expected_link_target: entry
                                .deployment
                                .previous_expected_link_target
                                .as_ref()
                                .map(PathBuf::from),
                            adapter_version: entry.target.adapter_id.clone(),
                            last_finalized_operation_id: plan.content.operation_id,
                            verified_at: journal.updated_at,
                        })
                        .map_err(|error| hook(error.to_string()))?;
                }
            }
            return Ok(());
        }
        if plan.content.batch_deployment.is_some() {
            for step in &plan.content.steps {
                let single = Self::single_plan(plan, step)?;
                self.publish_manifests(&single, journal)?;
            }
            return Ok(());
        }
        let context = Self::context(plan)?;
        let working = self.revalidate(plan)?;
        match context.action {
            DeploymentProductAction::Deploy => self
                .vault
                .manifests
                .write_deployment(&DeploymentManifest {
                    schema_version: 1,
                    deployment_id: context.deployment.deployment_id,
                    skill_id: context.skill.skill_id,
                    target_id: context.target.target_id,
                    deployment_name: context.skill.deployment_name.clone(),
                    mode: context.deployment.resolved_mode,
                    target_path: Path::new(&context.target.target_canonical_root)
                        .join(context.deployment.target_relative_path.as_str()),
                    expected_digest: context.skill.reviewed_digest,
                    expected_link_target: (context.deployment.resolved_mode
                        == DeploymentMode::Symlink)
                        .then_some(working),
                    adapter_version: context.target.adapter_id.clone(),
                    last_finalized_operation_id: plan.content.operation_id,
                    verified_at: journal.updated_at,
                })
                .map_err(|error| hook(error.to_string())),
            DeploymentProductAction::Undeploy => self
                .vault
                .manifests
                .remove_deployment(context.deployment.deployment_id)
                .map_err(|error| hook(error.to_string())),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn finalize_projection(
        &self,
        plan: &OperationPlan,
        journal: &crate::operations::OperationJournal,
    ) -> Result<(), OperationHookError> {
        if let Some(batch) = &plan.content.batch_deployment {
            let now = journal.updated_at;
            let active = batch.action == BatchDeploymentAction::Deploy;
            let deployments = batch
                .entries
                .iter()
                .map(|entry| DeploymentRecord {
                    id: entry.deployment.deployment_id,
                    skill_id: batch.skill.skill_id,
                    target_id: entry.target.target_id,
                    deployment_name: batch.skill.deployment_name.clone(),
                    target_path: Path::new(&entry.target.target_canonical_root)
                        .join(entry.deployment.target_relative_path.as_str()),
                    mode: if active
                        || entry
                            .inverse
                            .as_ref()
                            .is_some_and(|inverse| inverse.protected_reference.is_none())
                    {
                        entry.deployment.resolved_mode
                    } else if entry.deployment.previous_expected_link_target.is_some() {
                        DeploymentMode::Symlink
                    } else {
                        DeploymentMode::ManagedCopy
                    },
                    expected_digest: if active {
                        batch.skill.reviewed_digest
                    } else {
                        entry
                            .deployment
                            .previous_expected_digest
                            .unwrap_or(batch.skill.reviewed_digest)
                    },
                    expected_link_target: if active
                        && entry.deployment.resolved_mode == DeploymentMode::Symlink
                    {
                        Some(
                            self.vault
                                .paths
                                .root()
                                .join(batch.skill.working_bundle_path.as_str()),
                        )
                    } else {
                        entry
                            .deployment
                            .previous_expected_link_target
                            .as_ref()
                            .map(PathBuf::from)
                    },
                    health: if active {
                        DeploymentHealth::Clean
                    } else {
                        entry.deployment.reviewed_health
                    },
                    adapter_version: entry.target.adapter_id.clone(),
                    active: active
                        || entry
                            .inverse
                            .as_ref()
                            .is_some_and(|inverse| inverse.protected_reference.is_some()),
                    last_verified_at: Some(now),
                    last_operation_id: Some(plan.content.operation_id),
                    created_at: entry.deployment.deployment_created_at,
                    updated_at: now,
                })
                .collect();
            let evidence = batch
                .snapshot_id
                .map(|_| read_deployment_snapshot(&self.store, plan.content.operation_id))
                .transpose()?;
            let snapshot = batch.snapshot_id.map(|id| SnapshotRecord {
                id,
                operation_id: plan.content.operation_id,
                retention_state: "protected".into(),
                protected: true,
                created_at: plan.content.created_at,
            });
            let snapshot_items = evidence.map_or_else(Vec::new, |evidence| {
                evidence
                    .protections
                    .iter()
                    .enumerate()
                    .map(|(ordinal, protection)| SnapshotItemRecord {
                        snapshot_id: batch.snapshot_id.expect("evidence requires snapshot"),
                        ordinal,
                        digest: if batch.action == BatchDeploymentAction::Deploy {
                            protection.before.bundle_digest
                        } else {
                            None
                        },
                        entry_fingerprint: serde_json::to_value(&protection.before).ok(),
                        relation: "batch_deployment_target".into(),
                    })
                    .collect()
            });
            return self.vault.repositories.finalize_batch_deployment(BatchDeploymentProjection {
                operation: OperationRecord { id: plan.content.operation_id, plan_digest: plan.plan_digest.to_string(), operation_type: if active { "deploy".into() } else { "undo".into() }, state: OperationState::Finalized, outcome: Some(OperationOutcome::Succeeded), recovery_state: None, journal_path: BundleRelativePath::parse(&format!(".manager/operations/{}/journal.json", plan.content.operation_id)).map_err(|e| hook(e.to_string()))?, created_at: plan.content.created_at, updated_at: now, finalized_at: Some(now) },
                deployments, snapshot, snapshot_items,
                activity: ActivityRecord { id: batch.activity_id, operation_id: Some(plan.content.operation_id), kind: if active { "deploy".into() } else { "undo".into() }, state: "completed".into(), outcome: Some(OperationOutcome::Succeeded), summary: format!("{} {} at {} Targets", if active { "Deployed" } else { "Undid deployment of" }, batch.skill.deployment_name, batch.entries.len()), details: serde_json::json!({"skillId": batch.skill.skill_id, "targetIds": plan.content.selected_target_ids, "undoOf": batch.undo_of}), started_at: plan.content.created_at, completed_at: Some(now) },
            }).map_err(|e| hook(e.to_string()));
        }
        let context = Self::context(plan)?;
        let now = journal.updated_at;
        let active = context.action == DeploymentProductAction::Deploy;
        let expected_digest = if active {
            context.skill.reviewed_digest
        } else {
            context
                .deployment
                .previous_expected_digest
                .ok_or_else(|| hook("undeploy evidence has no previous expected digest"))?
        };
        let expected_link_target =
            if active && context.deployment.resolved_mode == DeploymentMode::Symlink {
                Some(
                    self.vault
                        .paths
                        .root()
                        .join(context.skill.working_bundle_path.as_str()),
                )
            } else {
                context
                    .deployment
                    .previous_expected_link_target
                    .as_ref()
                    .map(PathBuf::from)
            };
        let evidence = context
            .snapshot_id
            .map(|_| read_deployment_snapshot(&self.store, plan.content.operation_id))
            .transpose()?;
        let (snapshot, items) = match (context.snapshot_id, evidence) {
            (Some(snapshot_id), Some(evidence)) => (
                Some(SnapshotRecord {
                    id: snapshot_id,
                    operation_id: plan.content.operation_id,
                    retention_state: "protected".to_owned(),
                    protected: true,
                    created_at: plan.content.created_at,
                }),
                evidence
                    .protections
                    .iter()
                    .enumerate()
                    .map(|(ordinal, protection)| SnapshotItemRecord {
                        snapshot_id,
                        ordinal,
                        digest: protection.before.bundle_digest,
                        entry_fingerprint: serde_json::to_value(&protection.before).ok(),
                        relation: if context.action == DeploymentProductAction::Undeploy {
                            "undeploy_target".to_owned()
                        } else {
                            "redeploy_target".to_owned()
                        },
                    })
                    .collect(),
            ),
            _ => (None, Vec::new()),
        };
        let operation_type = if active { "deploy" } else { "undeploy" };
        self.vault
            .repositories
            .finalize_deployment(DeploymentProjection {
                operation: OperationRecord {
                    id: plan.content.operation_id,
                    plan_digest: plan.plan_digest.to_string(),
                    operation_type: operation_type.to_owned(),
                    state: OperationState::Finalized,
                    outcome: Some(OperationOutcome::Succeeded),
                    recovery_state: None,
                    journal_path: BundleRelativePath::parse(&format!(
                        ".manager/operations/{}/journal.json",
                        plan.content.operation_id
                    ))
                    .map_err(|error| hook(error.to_string()))?,
                    created_at: plan.content.created_at,
                    updated_at: now,
                    finalized_at: Some(now),
                },
                deployment: DeploymentRecord {
                    id: context.deployment.deployment_id,
                    skill_id: context.skill.skill_id,
                    target_id: context.target.target_id,
                    deployment_name: context.skill.deployment_name.clone(),
                    target_path: Path::new(&context.target.target_canonical_root)
                        .join(context.deployment.target_relative_path.as_str()),
                    mode: context.deployment.resolved_mode,
                    expected_digest,
                    expected_link_target,
                    health: if active {
                        DeploymentHealth::Clean
                    } else {
                        context.deployment.reviewed_health
                    },
                    adapter_version: context.target.adapter_id.clone(),
                    active,
                    last_verified_at: Some(now),
                    last_operation_id: Some(plan.content.operation_id),
                    created_at: context.deployment.deployment_created_at,
                    updated_at: now,
                },
                snapshot,
                snapshot_items: items,
                activity: ActivityRecord {
                    id: context.activity_id,
                    operation_id: Some(plan.content.operation_id),
                    kind: operation_type.to_owned(),
                    state: "completed".to_owned(),
                    outcome: Some(OperationOutcome::Succeeded),
                    summary: format!(
                        "{} {} at one Target",
                        if active { "Deployed" } else { "Undeployed" },
                        context.skill.deployment_name
                    ),
                    details: serde_json::json!({
                        "skillId": context.skill.skill_id,
                        "targetId": context.target.target_id,
                        "deploymentId": context.deployment.deployment_id,
                        "requestedMode": context.deployment.requested_mode,
                        "resolvedMode": context.deployment.resolved_mode,
                        "resolution": context.deployment.resolution,
                    }),
                    started_at: plan.content.created_at,
                    completed_at: Some(now),
                },
            })
            .map_err(|error| hook(error.to_string()))
    }
}

#[derive(Debug, Clone)]
struct HealthEvaluation {
    health: DeploymentHealth,
    explanation: String,
    vault_digest: Option<BundleDigest>,
    target_digest: Option<BundleDigest>,
    actual_link_target: Option<PathBuf>,
    verified_at: UtcTimestamp,
}

impl HealthEvaluation {
    fn unverified(explanation: impl Into<String>) -> Self {
        Self::simple(DeploymentHealth::Unverified, explanation)
    }

    fn conflict(explanation: impl Into<String>) -> Self {
        Self::simple(DeploymentHealth::Conflict, explanation)
    }

    fn simple(health: DeploymentHealth, explanation: impl Into<String>) -> Self {
        Self {
            health,
            explanation: explanation.into(),
            vault_digest: None,
            target_digest: None,
            actual_link_target: None,
            verified_at: UtcTimestamp::now(),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn evaluate_target(deployment: &DeploymentRecord, vault: Option<BundleDigest>) -> HealthEvaluation {
    let now = UtcTimestamp::now();
    match deployment.mode {
        DeploymentMode::ManagedCopy => {
            let metadata = match fs::symlink_metadata(&deployment.target_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return HealthEvaluation {
                        health: DeploymentHealth::MissingTarget,
                        explanation: "The managed target directory is missing.".to_owned(),
                        vault_digest: vault,
                        target_digest: None,
                        actual_link_target: None,
                        verified_at: now,
                    };
                }
                Err(_) => {
                    return HealthEvaluation {
                        health: DeploymentHealth::Unverified,
                        explanation: "The target cannot be inspected safely.".to_owned(),
                        vault_digest: vault,
                        target_digest: None,
                        actual_link_target: None,
                        verified_at: now,
                    };
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return HealthEvaluation {
                    health: DeploymentHealth::Conflict,
                    explanation: "The managed-copy path was replaced by another entry type."
                        .to_owned(),
                    vault_digest: vault,
                    target_digest: None,
                    actual_link_target: None,
                    verified_at: now,
                };
            }
            let target = match hash_bundle(&deployment.target_path, BundleCaps::default()) {
                Ok(hashed) => Some(hashed.digest),
                Err(_) => None,
            };
            let health = target.map_or(DeploymentHealth::Unverified, |target| {
                managed_copy_health(
                    deployment.expected_digest,
                    vault,
                    ManagedTargetObservation::Verified(target),
                )
            });
            HealthEvaluation {
                health,
                explanation: managed_explanation(health).to_owned(),
                vault_digest: vault,
                target_digest: target,
                actual_link_target: None,
                verified_at: now,
            }
        }
        DeploymentMode::Symlink => {
            let metadata = match fs::symlink_metadata(&deployment.target_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return HealthEvaluation {
                        health: DeploymentHealth::MissingTarget,
                        explanation: "The managed symbolic-link entry is missing.".to_owned(),
                        vault_digest: vault,
                        target_digest: None,
                        actual_link_target: None,
                        verified_at: now,
                    };
                }
                Err(_) => {
                    return HealthEvaluation::unverified("The link entry cannot be inspected.");
                }
            };
            if !metadata.file_type().is_symlink() {
                return HealthEvaluation {
                    health: DeploymentHealth::Conflict,
                    explanation: "The managed link was replaced by a regular entry.".to_owned(),
                    vault_digest: vault,
                    target_digest: None,
                    actual_link_target: None,
                    verified_at: now,
                };
            }
            let Ok(actual) = fs::read_link(&deployment.target_path) else {
                return HealthEvaluation::unverified("The raw link target is unreadable.");
            };
            if deployment.expected_link_target.as_ref() != Some(&actual) {
                return HealthEvaluation {
                    health: DeploymentHealth::Conflict,
                    explanation: "The managed link was retargeted away from the Vault.".to_owned(),
                    vault_digest: vault,
                    target_digest: None,
                    actual_link_target: Some(actual),
                    verified_at: now,
                };
            }
            if fs::metadata(&deployment.target_path).is_err() {
                return HealthEvaluation {
                    health: DeploymentHealth::BrokenLink,
                    explanation: "The link entry exists but its Vault target is unavailable."
                        .to_owned(),
                    vault_digest: vault,
                    target_digest: None,
                    actual_link_target: Some(actual),
                    verified_at: now,
                };
            }
            let health = symlink_health(
                deployment.expected_digest,
                vault,
                SymlinkTargetObservation::Correct,
            );
            HealthEvaluation {
                health,
                explanation: if health == DeploymentHealth::VaultAhead {
                    "Vault bytes changed and are already live through this link; explicit verification advances the expected digest."
                } else if health == DeploymentHealth::Clean {
                    "The absolute link and current Vault digest match the last verified deployment."
                } else {
                    "The Vault content cannot be verified."
                }
                .to_owned(),
                vault_digest: vault,
                target_digest: None,
                actual_link_target: Some(actual),
                verified_at: now,
            }
        }
    }
}

fn managed_explanation(health: DeploymentHealth) -> &'static str {
    match health {
        DeploymentHealth::Clean => "Vault, target, and expected digests match.",
        DeploymentHealth::VaultAhead => {
            "Vault changed while the managed target still matches the last verified digest."
        }
        DeploymentHealth::TargetModified => {
            "The target changed while Vault still matches the last verified digest."
        }
        DeploymentHealth::Conflict => "Vault and target diverged from the last verified digest.",
        DeploymentHealth::MissingTarget => "The managed target is missing.",
        DeploymentHealth::BrokenLink => "The managed link is broken.",
        DeploymentHealth::Unverified => "Current target evidence cannot prove a safe relationship.",
    }
}

fn health_view(record: &DeploymentRecord, evaluation: HealthEvaluation) -> DeploymentHealthView {
    let (actions, disabled) = allowed_actions(evaluation.health, record.active);
    DeploymentHealthView {
        deployment_id: record.id.to_string(),
        skill_id: record.skill_id.to_string(),
        target_id: record.target_id.to_string(),
        deployment_name: record.deployment_name.to_string(),
        target_path: record.target_path.to_string_lossy().into_owned(),
        mode: mode_dto(record.mode),
        active: record.active,
        health: health_text(evaluation.health).to_owned(),
        explanation: evaluation.explanation,
        expected_digest: record.expected_digest.to_string(),
        vault_digest: evaluation.vault_digest.map(|digest| digest.to_string()),
        target_digest: evaluation.target_digest.map(|digest| digest.to_string()),
        expected_link_target: record
            .expected_link_target
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        actual_link_target: evaluation
            .actual_link_target
            .map(|path| path.to_string_lossy().into_owned()),
        drift_direction: drift_direction(evaluation.health).to_owned(),
        allowed_actions: actions,
        disabled_reason: disabled,
        verified_at: evaluation.verified_at.to_string(),
    }
}

fn allowed_actions(health: DeploymentHealth, active: bool) -> (Vec<String>, Option<String>) {
    if !active {
        return (Vec::new(), Some("Deployment is inactive.".to_owned()));
    }
    match health {
        DeploymentHealth::Clean => (vec!["verify".into(), "undeploy".into()], None),
        DeploymentHealth::VaultAhead => (
            vec![
                "verify".into(),
                "redeploy".into(),
                "undeploy_preserve".into(),
            ],
            None,
        ),
        DeploymentHealth::TargetModified | DeploymentHealth::Conflict => (
            vec!["verify".into(), "undeploy_preserve".into()],
            Some("Changed target bytes cannot be overwritten or deleted silently.".to_owned()),
        ),
        DeploymentHealth::MissingTarget => {
            (vec!["verify".into(), "undeploy_preserve".into()], None)
        }
        DeploymentHealth::BrokenLink => (
            vec!["verify".into(), "undeploy_preserve".into()],
            Some(
                "Repair and removal are blocked; preserve can end only the managed relationship without following the broken link."
                    .to_owned(),
            ),
        ),
        DeploymentHealth::Unverified => (
            vec!["verify".into()],
            Some("Unreadable evidence blocks mutation.".to_owned()),
        ),
    }
}

fn drift_direction(health: DeploymentHealth) -> &'static str {
    match health {
        DeploymentHealth::Clean => "none",
        DeploymentHealth::VaultAhead => "vault_to_target",
        DeploymentHealth::TargetModified => "target_only",
        DeploymentHealth::Conflict => "both",
        DeploymentHealth::MissingTarget => "target_missing",
        DeploymentHealth::BrokenLink => "link_broken",
        DeploymentHealth::Unverified => "unknown",
    }
}

fn current_fingerprint(
    target: &TargetRecord,
    deployment: &DeploymentRecord,
    evaluation: &HealthEvaluation,
    captured_at: UtcTimestamp,
) -> Result<PathFingerprint, DeploymentError> {
    let metadata = match fs::symlink_metadata(&deployment.target_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(fingerprint(
                target,
                EntryKind::Absent,
                None,
                None,
                None,
                None,
                Some(deployment.skill_id),
                Some(deployment.id),
                captured_at,
            ));
        }
        Err(error) => return Err(DeploymentError::Io(error)),
    };
    let kind = if metadata.file_type().is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Unsupported
    };
    let raw_symlink_target = (kind == EntryKind::Symlink)
        .then(|| fs::read_link(&deployment.target_path))
        .transpose()?;
    let resolves_reviewed_vault = matches!(
        evaluation.health,
        DeploymentHealth::Clean | DeploymentHealth::VaultAhead
    ) && raw_symlink_target.as_ref()
        == deployment.expected_link_target.as_ref();
    let raw_symlink_target = raw_symlink_target
        .as_deref()
        .map(|path| exact_plan_path(path, "current raw symlink target"))
        .transpose()?;
    Ok(fingerprint(
        target,
        kind,
        raw_symlink_target,
        Some(MetadataFingerprint::from_metadata(&metadata)),
        (kind == EntryKind::Directory)
            .then_some(evaluation.target_digest)
            .flatten(),
        (kind == EntryKind::Symlink && resolves_reviewed_vault)
            .then_some(evaluation.vault_digest)
            .flatten(),
        Some(deployment.skill_id),
        Some(deployment.id),
        captured_at,
    ))
}

fn exact_plan_path(path: &Path, evidence: &'static str) -> Result<String, DeploymentError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        DeploymentError::DriftBlocked(format!(
            "{evidence} is not valid UTF-8, so exact Operation evidence cannot be sealed"
        ))
    })
}

fn desired_fingerprint(
    target: &TargetRecord,
    skill_id: SkillId,
    deployment_id: DeploymentId,
    mode: DeploymentMode,
    digest: BundleDigest,
    working: &Path,
    captured_at: UtcTimestamp,
) -> Result<PathFingerprint, DeploymentError> {
    Ok(match mode {
        DeploymentMode::ManagedCopy => fingerprint(
            target,
            EntryKind::Directory,
            None,
            None,
            Some(digest),
            None,
            Some(skill_id),
            Some(deployment_id),
            captured_at,
        ),
        DeploymentMode::Symlink => fingerprint(
            target,
            EntryKind::Symlink,
            Some(
                working
                    .to_str()
                    .ok_or(DeploymentError::InvalidTargetDirectory)?
                    .to_owned(),
            ),
            None,
            None,
            Some(digest),
            Some(skill_id),
            Some(deployment_id),
            captured_at,
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn fingerprint(
    target: &TargetRecord,
    expected_kind: EntryKind,
    raw_symlink_target: Option<String>,
    metadata: Option<MetadataFingerprint>,
    bundle_digest: Option<BundleDigest>,
    resolved_bundle_digest: Option<BundleDigest>,
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
        resolved_bundle_digest,
        managed_skill_id,
        managed_deployment_id,
        captured_at,
        adapter_id: target.adapter_id.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn deployment_context(
    action: DeploymentProductAction,
    skill: &crate::persistence::SkillRecord,
    target: &TargetRecord,
    project: Option<&ProjectRecord>,
    capability: TargetCapabilityEvidence,
    deployment_id: DeploymentId,
    existing_deployment: bool,
    requested_mode: DeploymentMode,
    resolved_mode: DeploymentMode,
    fallback_reason: Option<String>,
    previous_expected_digest: Option<BundleDigest>,
    previous_expected_link_target: Option<String>,
    reviewed_health: DeploymentHealth,
    resolution: Option<UndeployResolution>,
    deployment_created_at: UtcTimestamp,
    deployment_updated_at: UtcTimestamp,
    reviewed_digest: BundleDigest,
    snapshot_id: Option<SnapshotId>,
    vault_root: &Path,
) -> Result<DeploymentPlanContext, DeploymentError> {
    Ok(DeploymentPlanContext {
        action,
        skill: DeploymentSkillEvidence {
            skill_id: skill.id,
            deployment_name: skill.deployment_name.clone(),
            vault_root: vault_root
                .to_str()
                .ok_or(DeploymentError::InvalidTargetDirectory)?
                .to_owned(),
            working_bundle_path: skill.working_path.clone(),
            reviewed_digest,
        },
        target: DeploymentTargetEvidence {
            target_id: target.id,
            adapter_id: target.adapter_id.clone(),
            target_scope: if target.scope == "project" {
                TakeoverTargetScope::Project
            } else {
                TakeoverTargetScope::Global
            },
            target_root: target
                .root_path
                .to_str()
                .ok_or(DeploymentError::InvalidTargetDirectory)?
                .to_owned(),
            target_canonical_root: target
                .canonical_root_path
                .to_str()
                .ok_or(DeploymentError::InvalidTargetDirectory)?
                .to_owned(),
            project_id: target.project_id,
            project_git_classification: project.map(|value| value.git_classification.clone()),
            is_override: target.is_override,
            is_custom: target.is_custom,
            capability,
        },
        deployment: ManagedDeploymentEvidence {
            deployment_id,
            existing_deployment,
            active_before: existing_deployment,
            target_relative_path: BundleRelativePath::parse(skill.deployment_name.as_str())
                .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?,
            requested_mode,
            resolved_mode,
            fallback_reason,
            previous_expected_digest,
            previous_expected_link_target,
            reviewed_health,
            resolution,
            step_order: 0,
            manifest_path: BundleRelativePath::parse(&format!(
                ".manager/manifests/deployments/{deployment_id}.json"
            ))
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?,
            deployment_created_at,
            deployment_updated_at,
        },
        activity_id: ActivityId::generate(),
        snapshot_id,
    })
}

fn deployment_plan_view(plan: &OperationPlan) -> Result<DeploymentPlanView, DeploymentError> {
    if plan.content.deployment.is_none() && plan.content.batch_deployment.is_some() {
        let step =
            plan.content.steps.first().ok_or_else(|| {
                DeploymentError::Journal("Operation has no deployment step".into())
            })?;
        let single = DeploymentHooks::single_plan(plan, step)
            .map_err(|error| DeploymentError::Journal(error.to_string()))?;
        let mut view = deployment_plan_view(&single)?;
        view.operation_id = plan.content.operation_id.to_string();
        view.plan_digest = plan.plan_digest.to_string();
        return Ok(view);
    }
    let context =
        plan.content.deployment.as_ref().ok_or_else(|| {
            DeploymentError::Journal("Operation has no deployment context".into())
        })?;
    let step = plan
        .content
        .steps
        .get(context.deployment.step_order as usize)
        .ok_or_else(|| DeploymentError::Journal("Operation has no deployment step".into()))?;
    Ok(DeploymentPlanView {
        operation_id: plan.content.operation_id.to_string(),
        plan_digest: plan.plan_digest.to_string(),
        expires_at: plan.content.expires_at.to_string(),
        action: match context.action {
            DeploymentProductAction::Deploy => "deploy",
            DeploymentProductAction::Undeploy => "undeploy",
        }
        .to_owned(),
        skill_id: context.skill.skill_id.to_string(),
        target_id: context.target.target_id.to_string(),
        deployment_id: context.deployment.deployment_id.to_string(),
        target_path: step.path.display_path().to_owned(),
        requested_mode: mode_dto(context.deployment.requested_mode),
        resolved_mode: mode_dto(context.deployment.resolved_mode),
        fallback_reason: context.deployment.fallback_reason.clone(),
        reviewed_health: health_text(context.deployment.reviewed_health).to_owned(),
        no_op: step.action == PlanAction::LeaveUntouched,
        consequence: plan_consequence(context, step),
        recovery_count: plan.content.recovery.snapshot_count,
        execution_allowed: plan.content.blockers.is_empty(),
    })
}

fn batch_deployment_plan_view(
    plan: &OperationPlan,
) -> Result<BatchDeploymentPlanView, DeploymentError> {
    let context = plan.content.batch_deployment.as_ref().ok_or_else(|| {
        DeploymentError::Journal("Operation has no batch deployment context".into())
    })?;
    let mut entries = Vec::with_capacity(context.entries.len());
    for step in &plan.content.steps {
        let entry = context
            .entries
            .iter()
            .find(|entry| entry.deployment.step_order == step.order)
            .ok_or_else(|| DeploymentError::Journal("Operation has no batch entry".into()))?;
        entries.push(DeploymentPlanView {
            operation_id: plan.content.operation_id.to_string(),
            plan_digest: plan.plan_digest.to_string(),
            expires_at: plan.content.expires_at.to_string(),
            action: if context.action == BatchDeploymentAction::Deploy {
                "deploy"
            } else {
                "undo"
            }
            .into(),
            skill_id: context.skill.skill_id.to_string(),
            target_id: entry.target.target_id.to_string(),
            deployment_id: entry.deployment.deployment_id.to_string(),
            target_path: step.path.display_path().to_owned(),
            requested_mode: mode_dto(entry.deployment.requested_mode),
            resolved_mode: mode_dto(entry.deployment.resolved_mode),
            fallback_reason: entry.deployment.fallback_reason.clone(),
            reviewed_health: health_text(entry.deployment.reviewed_health).into(),
            no_op: step.action == PlanAction::LeaveUntouched,
            consequence: "Execute one entry of the reviewed batch transaction.".into(),
            recovery_count: plan.content.recovery.snapshot_count,
            execution_allowed: plan.content.blockers.is_empty(),
        });
    }
    Ok(BatchDeploymentPlanView {
        operation_id: plan.content.operation_id.to_string(),
        plan_digest: plan.plan_digest.to_string(),
        expires_at: plan.content.expires_at.to_string(),
        action: if context.action == BatchDeploymentAction::Deploy {
            "deploy".into()
        } else {
            "undo".into()
        },
        skill_id: context.skill.skill_id.to_string(),
        entries,
        recovery_count: plan.content.recovery.snapshot_count,
        consequence: format!(
            "One reviewed transaction across {} Targets",
            context.entries.len()
        ),
        execution_allowed: plan.content.blockers.is_empty(),
    })
}

fn plan_consequence(context: &DeploymentPlanContext, step: &PlanStep) -> String {
    match (context.action, step.action, context.deployment.resolved_mode) {
        (DeploymentProductAction::Deploy, PlanAction::LeaveUntouched, DeploymentMode::Symlink) => {
            "No target write. The absolute link already exposes current Vault bytes; execution only re-verifies and advances durable expected evidence.".to_owned()
        }
        (DeploymentProductAction::Deploy, PlanAction::LeaveUntouched, _) => {
            "No target write; current deployment already matches the reviewed Vault version."
                .to_owned()
        }
        (DeploymentProductAction::Deploy, _, DeploymentMode::Symlink) => {
            "Create one absolute directory link to the reviewed Vault working Bundle.".to_owned()
        }
        (DeploymentProductAction::Deploy, _, DeploymentMode::ManagedCopy) => {
            "Stage and atomically activate an exact managed copy of the reviewed Vault Bundle."
                .to_owned()
        }
        (DeploymentProductAction::Undeploy, PlanAction::Remove, _) => {
            "Protect and remove exactly this managed target; Vault and other deployments remain unchanged."
                .to_owned()
        }
        (DeploymentProductAction::Undeploy, PlanAction::LeaveUntouched, _) => {
            "Preserve changed target bytes and end only this managed relationship.".to_owned()
        }
        _ => "Execute the reviewed single-target operation.".to_owned(),
    }
}

fn manifest_matches(manifest: &DeploymentManifest, record: &DeploymentRecord) -> bool {
    manifest.deployment_id == record.id
        && manifest.skill_id == record.skill_id
        && manifest.target_id == record.target_id
        && manifest.deployment_name == record.deployment_name
        && manifest.mode == record.mode
        && manifest.target_path == record.target_path
        && manifest.expected_digest == record.expected_digest
        && manifest.expected_link_target == record.expected_link_target
        && manifest.adapter_version == record.adapter_version
}

fn resolve_mode(
    requested: DeploymentMode,
    capability: &TargetCapabilityEvidence,
) -> Result<(DeploymentMode, Option<String>), DeploymentError> {
    match (requested, capability.symlink) {
        (DeploymentMode::Symlink, CapabilityStatus::Supported) => {
            Ok((DeploymentMode::Symlink, None))
        }
        (DeploymentMode::Symlink, CapabilityStatus::Unsupported) => Ok((
            DeploymentMode::ManagedCopy,
            Some(
                "Target preflight proved that absolute directory symlinks are unsupported; the new plan stages a Managed Copy and requires confirmation."
                    .to_owned(),
            ),
        )),
        (DeploymentMode::Symlink, CapabilityStatus::Unknown) => Err(
            DeploymentError::CapabilityBlocked("symlink capability is unknown".to_owned()),
        ),
        (DeploymentMode::ManagedCopy, _) => Ok((DeploymentMode::ManagedCopy, None)),
    }
}

fn require_base_capability(capability: &TargetCapabilityEvidence) -> Result<(), DeploymentError> {
    if capability.directory_write == CapabilityStatus::Supported
        && capability.atomic_rename == CapabilityStatus::Supported
    {
        Ok(())
    } else {
        Err(DeploymentError::CapabilityBlocked(
            "directory write and same-parent atomic rename must both be Supported".to_owned(),
        ))
    }
}

fn default_mode(
    target: &TargetRecord,
    project: Option<&ProjectRecord>,
) -> Result<DeploymentMode, DeploymentError> {
    if target.is_custom {
        return Ok(DeploymentMode::Symlink);
    }
    match (target.scope.as_str(), project) {
        ("global", None) => Ok(DeploymentMode::Symlink),
        ("project", Some(project)) if project.git_classification == "git" => {
            Ok(DeploymentMode::ManagedCopy)
        }
        ("project", Some(project)) if project.git_classification == "none" => {
            Ok(DeploymentMode::Symlink)
        }
        _ => Err(DeploymentError::TargetMissing),
    }
}

pub(crate) fn configured_override_matches(
    repositories: &Repositories,
    adapter_id: &AdapterId,
    scope: &str,
    project_id: Option<ProjectId>,
    canonical_root: &Path,
) -> Result<bool, DeploymentError> {
    let Some(descriptor) = crate::adapters::descriptor(adapter_id) else {
        return if crate::adapters::DESCRIPTORS
            .into_iter()
            .any(|known| known.name == adapter_id.name())
        {
            Err(DeploymentError::TargetMissing)
        } else {
            Ok(false)
        };
    };
    let configuration = repositories
        .adapter_configurations()?
        .into_iter()
        .find(|row| row.adapter_name == descriptor.name);
    let Some(configuration) = configuration else {
        return Ok(false);
    };
    if !configuration.enabled || configuration.adapter_id != *adapter_id {
        return Err(DeploymentError::TargetMissing);
    }
    match scope {
        "global" => Ok(configuration
            .global_override_path
            .is_some_and(|root| root == canonical_root)),
        "project" => {
            let Some(relative) = configuration.project_override_path else {
                return Ok(false);
            };
            let project = project_id
                .map(|id| repositories.project(id))
                .transpose()?
                .flatten()
                .ok_or(DeploymentError::TargetMissing)?;
            Ok(project.canonical_path.join(relative) == canonical_root)
        }
        _ => Err(DeploymentError::TargetMissing),
    }
}

pub(crate) fn ensure_target_is_configured(
    repositories: &Repositories,
    target: &TargetRecord,
) -> Result<(), DeploymentError> {
    if target.is_custom {
        return Ok(());
    }
    if crate::adapters::descriptor(&target.adapter_id).is_none()
        && !crate::adapters::DESCRIPTORS
            .into_iter()
            .any(|known| known.name == target.adapter_id.name())
    {
        return Ok(());
    }
    let is_current_override = configured_override_matches(
        repositories,
        &target.adapter_id,
        &target.scope,
        target.project_id,
        &target.canonical_root_path,
    )?;
    match (target.is_override, is_current_override) {
        (true, true) | (false, _) => Ok(()),
        (true, false) => Err(DeploymentError::TargetMissing),
    }
}

fn target_view(
    target: &TargetRecord,
    project: Option<&ProjectRecord>,
) -> Result<TargetView, DeploymentError> {
    Ok(target_view_with_mode(
        target,
        project,
        default_mode(target, project)?,
    ))
}

fn target_view_with_mode(
    target: &TargetRecord,
    project: Option<&ProjectRecord>,
    default_mode: DeploymentMode,
) -> TargetView {
    TargetView {
        target_id: target.id.to_string(),
        adapter_id: target.adapter_id.to_string(),
        scope: target.scope.clone(),
        project_id: target.project_id.map(|id| id.to_string()),
        project_kind: project.map(|project| project.git_classification.clone()),
        root_path: target.root_path.to_string_lossy().into_owned(),
        is_override: target.is_override,
        is_custom: target.is_custom,
        default_mode: mode_dto(default_mode),
    }
}

fn project_kind_text(kind: FixtureTargetKindDto) -> &'static str {
    match kind {
        FixtureTargetKindDto::GitProject => "git",
        FixtureTargetKindDto::PersonalProject | FixtureTargetKindDto::Global => "none",
    }
}

fn target_scope_text(scope: TakeoverTargetScope) -> &'static str {
    match scope {
        TakeoverTargetScope::Global => "global",
        TakeoverTargetScope::Project => "project",
    }
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

fn health_text(value: DeploymentHealth) -> &'static str {
    match value {
        DeploymentHealth::Clean => "clean",
        DeploymentHealth::VaultAhead => "vault_ahead",
        DeploymentHealth::TargetModified => "target_modified",
        DeploymentHealth::MissingTarget => "missing_target",
        DeploymentHealth::BrokenLink => "broken_link",
        DeploymentHealth::Conflict => "conflict",
        DeploymentHealth::Unverified => "unverified",
    }
}

fn adapter_id() -> Result<AdapterId, DeploymentError> {
    AdapterId::from_str(UNIVERSAL_ADAPTER).map_err(|error| DeploymentError::InvalidId {
        entity: "Adapter",
        detail: error.to_string(),
    })
}

fn parse_id<T>(value: &str, entity: &'static str) -> Result<T, DeploymentError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error: T::Err| DeploymentError::InvalidId {
            entity,
            detail: error.to_string(),
        })
}

fn ensure_disjoint(vault: &Path, target: &Path) -> Result<(), DeploymentError> {
    let vault = vault
        .canonicalize()
        .map_err(|_| DeploymentError::InvalidTargetDirectory)?;
    let target = target
        .canonicalize()
        .map_err(|_| DeploymentError::InvalidTargetDirectory)?;
    if vault.starts_with(&target) || target.starts_with(&vault) {
        Err(DeploymentError::InvalidTargetDirectory)
    } else {
        Ok(())
    }
}

fn ensure_no_name_collision(
    target_root: &Path,
    deployment_name: &DeploymentName,
    reviewed_path: &Path,
) -> Result<(), DeploymentError> {
    let collision_key = deployment_name.collision_key();
    for entry in fs::read_dir(target_root)? {
        let entry = entry?;
        if entry.path() == reviewed_path {
            continue;
        }
        let name = entry.file_name().into_string().map_err(|_| {
            DeploymentError::CapabilityBlocked(
                "target contains a non-UTF-8 name whose collision identity cannot be proven"
                    .to_owned(),
            )
        })?;
        if normalized_collision_key(&name) == collision_key {
            return Err(DeploymentError::UnmanagedCollision);
        }
    }
    Ok(())
}

fn same_persisted_time(left: UtcTimestamp, right: UtcTimestamp) -> bool {
    left.unix_millis().ok() == right.unix_millis().ok()
}

fn deployment_snapshot_path(store: &OperationStore, operation_id: OperationId) -> PathBuf {
    store
        .operation_directory(operation_id)
        .join("deployment-snapshot.json")
}

fn read_deployment_snapshot(
    store: &OperationStore,
    operation_id: OperationId,
) -> Result<DeploymentSnapshotEvidence, OperationHookError> {
    serde_json::from_slice(
        &fs::read(deployment_snapshot_path(store, operation_id))
            .map_err(|error| hook(error.to_string()))?,
    )
    .map_err(|error| hook(error.to_string()))
}

fn read_deployment_snapshot_for_planning(
    store: &OperationStore,
    operation_id: OperationId,
) -> Result<DeploymentSnapshotEvidence, DeploymentError> {
    serde_json::from_slice(&fs::read(deployment_snapshot_path(store, operation_id))?)
        .map_err(|error| DeploymentError::Journal(error.to_string()))
}

fn verify_protected_reference(
    vault: &OpenVault,
    protection: &SnapshotProtection,
    caps: BundleCaps,
) -> Result<(), DeploymentError> {
    if let Some(encoded) = protection.reference.strip_prefix("object:") {
        let digest = BundleDigest::from_str(encoded)
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?;
        if protection.before.bundle_digest != Some(digest) {
            return Err(DeploymentError::DriftBlocked(
                "protected object does not match sealed before fingerprint".into(),
            ));
        }
        vault
            .objects
            .verify(digest)
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?;
        let actual = hash_bundle(&vault.objects.object_path(digest).join("bundle"), caps)
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?;
        if actual.digest != digest {
            return Err(DeploymentError::DriftBlocked(
                "protected object bytes changed".into(),
            ));
        }
    } else if let Some(target) = protection.reference.strip_prefix("link:") {
        if protection.before.raw_symlink_target.as_deref() != Some(target) {
            return Err(DeploymentError::DriftBlocked(
                "protected link reference changed".into(),
            ));
        }
    } else {
        return Err(DeploymentError::DriftBlocked(
            "unsupported protected Snapshot reference".into(),
        ));
    }
    Ok(())
}

fn verify_sealed_postcondition(step: &PlanStep, caps: BundleCaps) -> Result<(), DeploymentError> {
    let path = Path::new(step.path.display_path());
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        DeploymentError::DriftBlocked(format!("sealed batch postcondition changed: {error}"))
    })?;
    let kind = if metadata.file_type().is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Unsupported
    };
    if kind != step.after.expected_kind {
        return Err(DeploymentError::DriftBlocked(
            "sealed batch postcondition entry kind changed".into(),
        ));
    }
    if kind == EntryKind::Directory {
        let digest = hash_bundle(path, caps)
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?
            .digest;
        if step.after.bundle_digest != Some(digest) {
            return Err(DeploymentError::DriftBlocked(
                "sealed batch postcondition bytes changed".into(),
            ));
        }
    } else if kind == EntryKind::Symlink {
        let raw = fs::read_link(path)?;
        if raw.to_str() != step.after.raw_symlink_target.as_deref() {
            return Err(DeploymentError::DriftBlocked(
                "sealed batch postcondition link changed".into(),
            ));
        }
        if let Some(expected) = step.after.resolved_bundle_digest {
            let digest = hash_bundle(
                Path::new(
                    step.after
                        .raw_symlink_target
                        .as_deref()
                        .expect("link target"),
                ),
                caps,
            )
            .map_err(|error| DeploymentError::DriftBlocked(error.to_string()))?
            .digest;
            if digest != expected {
                return Err(DeploymentError::DriftBlocked(
                    "sealed batch resolved postcondition changed".into(),
                ));
            }
        }
    }
    Ok(())
}

fn hook(message: impl Into<String>) -> OperationHookError {
    OperationHookError::new(message.into())
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::{Duration, Instant},
    };

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{
        application::activity::ActivityQuery,
        application::takeover::{TakeoverDecisionDto, TakeoverPlanRequest, TakeoverService},
        domain::{DeploymentName, ObservationId, normalized_path_identity},
        operations::{OperationBoundary, StartupDecision, classify_startup},
        persistence::ObservationRecord,
    };

    struct Fixture {
        temporary: TempDir,
        vault: Arc<OpenVault>,
        service: DeploymentService,
        skill_id: SkillId,
        working: PathBuf,
    }

    fn fixture() -> Fixture {
        let temporary = tempdir().unwrap();
        let vault_root = temporary.path().join("vault");
        let support = temporary.path().join("support");
        let source = temporary.path().join("external/example");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "reviewed\n").unwrap();
        fs::write(source.join("tool.sh"), "#!/bin/sh\necho reviewed\n").unwrap();
        let vault = Arc::new(OpenVault::open(&vault_root, &support, &[]).unwrap());
        let observation_id = ObservationId::generate();
        let now = UtcTimestamp::now();
        vault
            .repositories
            .upsert_observation(ObservationRecord {
                id: observation_id,
                skill_id: None,
                adapter_id: adapter_id().unwrap(),
                scope: "global".into(),
                project_id: None,
                source_root_kind: "fixture".into(),
                source_root_id: "fixture-global".into(),
                display_path: source.clone(),
                normalized_path: normalized_path_identity(source.to_str().unwrap()),
                canonical_path: Some(source.canonicalize().unwrap()),
                deployment_name: DeploymentName::parse("example").unwrap(),
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
        let coordinator = Arc::new(OperationCoordinator::new());
        let takeover = TakeoverService::with_runtime(Arc::clone(&vault), Arc::clone(&coordinator));
        let plan = takeover
            .plan_takeover(TakeoverPlanRequest {
                source_observation_id: observation_id.to_string(),
                decision: TakeoverDecisionDto::AddToVault,
                selected_locations: Vec::new(),
            })
            .unwrap();
        takeover
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap();
        let skill_id = SkillId::from_str(&plan.skill_id).unwrap();
        let working = vault.paths.root().join(&plan.working_path);
        Fixture {
            temporary,
            vault: Arc::clone(&vault),
            service: DeploymentService::with_runtime(vault, coordinator),
            skill_id,
            working,
        }
    }

    fn target(fixture: &Fixture, name: &str, kind: FixtureTargetKindDto) -> TargetView {
        let root = fixture.temporary.path().join(name);
        fs::create_dir(&root).unwrap();
        fixture
            .service
            .register_target(&RegisterTargetRequest {
                kind,
                selected_directory: root.to_string_lossy().into_owned(),
                adapter_id: None,
                is_override: None,
            })
            .unwrap()
    }

    fn plan(
        fixture: &Fixture,
        target: &TargetView,
        requested_mode: Option<DeploymentModeDto>,
    ) -> DeploymentPlanView {
        fixture
            .service
            .plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: target.target_id.clone(),
                requested_mode,
            })
            .unwrap()
    }

    fn execute(fixture: &Fixture, plan: &DeploymentPlanView) -> DeploymentOperationView {
        fixture
            .service
            .execute_operation(&plan.operation_id, &plan.plan_digest)
            .unwrap()
    }

    fn operation_activity_counts(fixture: &Fixture, operation_id: OperationId) -> (i64, i64) {
        fixture
            .vault
            .database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT
                            (SELECT count(*) FROM operations WHERE id = ?1),
                            (SELECT count(*) FROM activity WHERE operation_id = ?1)",
                        [operation_id.to_string()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)
            })
            .unwrap()
    }

    fn projection_totals(fixture: &Fixture) -> (i64, i64) {
        fixture
            .vault
            .database
            .execute(|connection| {
                connection
                    .query_row(
                        "SELECT
                            (SELECT count(*) FROM operations),
                            (SELECT count(*) FROM activity)",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)
            })
            .unwrap()
    }

    fn assert_projected_success(fixture: &Fixture, operation_id: OperationId) {
        let projected: (String, String) = fixture
            .vault
            .database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT state, outcome FROM operations WHERE id = ?1",
                        [operation_id.to_string()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)
            })
            .unwrap();
        assert_eq!(projected, ("finalized".to_owned(), "succeeded".to_owned()));
        assert_eq!(operation_activity_counts(fixture, operation_id), (1, 1));
    }

    fn advance_working(fixture: &Fixture, body: &str) -> BundleDigest {
        fs::write(fixture.working.join("SKILL.md"), body).unwrap();
        let digest = hash_bundle(&fixture.working, BundleCaps::default())
            .unwrap()
            .digest;
        let mut skill = fixture
            .vault
            .repositories
            .skill(fixture.skill_id)
            .unwrap()
            .unwrap();
        skill.working_digest = digest;
        skill.updated_at = UtcTimestamp::now();
        fixture.vault.repositories.upsert_skill(skill).unwrap();
        let mut manifest = fixture
            .vault
            .manifests
            .read_skill(fixture.skill_id)
            .unwrap();
        manifest.working_digest = digest;
        fixture.vault.manifests.write_skill(&manifest).unwrap();
        digest
    }

    #[derive(Clone)]
    struct FixedProbe(TargetCapabilityEvidence);

    impl CapabilityProbe for FixedProbe {
        fn inspect(
            &self,
            _target_root: &Path,
            _link_target: &Path,
        ) -> Result<TargetCapabilityEvidence, DeploymentError> {
            Ok(self.0.clone())
        }
    }

    struct ChangeProbe {
        calls: AtomicUsize,
        supported_calls: usize,
    }

    impl CapabilityProbe for ChangeProbe {
        fn inspect(
            &self,
            _target_root: &Path,
            _link_target: &Path,
        ) -> Result<TargetCapabilityEvidence, DeploymentError> {
            let supported = self.calls.fetch_add(1, Ordering::SeqCst) < self.supported_calls;
            Ok(TargetCapabilityEvidence {
                directory_write: CapabilityStatus::Supported,
                atomic_rename: CapabilityStatus::Supported,
                symlink: if supported {
                    CapabilityStatus::Supported
                } else {
                    CapabilityStatus::Unsupported
                },
            })
        }
    }

    struct FailAt(OperationBoundary);

    impl OperationFailpoints for FailAt {
        fn check(&self, boundary: OperationBoundary) -> Result<(), OperationHookError> {
            if boundary == self.0 {
                Err(hook(format!("injected deployment failure at {boundary:?}")))
            } else {
                Ok(())
            }
        }
    }

    struct CrashAt {
        boundary: OperationBoundary,
        marker: PathBuf,
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn mixed_batch_replace_and_create_undo_restores_exact_targets_and_relationships() {
        let fixture = fixture();
        let existing_target = target(&fixture, "batch-existing", FixtureTargetKindDto::GitProject);
        let new_target = target(&fixture, "batch-new", FixtureTargetKindDto::Global);
        let initial = plan(
            &fixture,
            &existing_target,
            Some(DeploymentModeDto::ManagedCopy),
        );
        execute(&fixture, &initial);
        let existing_id = DeploymentId::from_str(&initial.deployment_id).unwrap();
        let before_digest = hash_bundle(Path::new(&initial.target_path), BundleCaps::default())
            .unwrap()
            .digest;
        let before_record = fixture
            .vault
            .repositories
            .deployment(existing_id)
            .unwrap()
            .unwrap();
        advance_working(&fixture, "vault ahead for mixed batch\n");

        let batch = fixture
            .service
            .plan_batch_deployment(&BatchDeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                targets: vec![
                    BatchDeploymentTargetChoice {
                        target_id: existing_target.target_id.clone(),
                        requested_mode: Some(DeploymentModeDto::ManagedCopy),
                    },
                    BatchDeploymentTargetChoice {
                        target_id: new_target.target_id.clone(),
                        requested_mode: Some(DeploymentModeDto::Symlink),
                    },
                ],
            })
            .unwrap();
        assert!(
            batch
                .entries
                .iter()
                .any(|entry| { entry.resolved_mode == DeploymentModeDto::ManagedCopy })
        );
        assert!(
            batch
                .entries
                .iter()
                .any(|entry| { entry.resolved_mode == DeploymentModeDto::Symlink })
        );
        fixture
            .service
            .execute_any_operation(&batch.operation_id, &batch.plan_digest)
            .unwrap();
        let undo = fixture.service.plan_undo(&batch.operation_id).unwrap();
        fixture
            .service
            .execute_any_operation(&undo.operation_id, &undo.plan_digest)
            .unwrap();

        assert_eq!(
            hash_bundle(Path::new(&initial.target_path), BundleCaps::default())
                .unwrap()
                .digest,
            before_digest
        );
        let restored = fixture
            .vault
            .repositories
            .deployment(existing_id)
            .unwrap()
            .unwrap();
        assert!(restored.active);
        assert_eq!(restored.mode, before_record.mode);
        assert_eq!(restored.expected_digest, before_record.expected_digest);
        assert_eq!(
            restored.expected_link_target,
            before_record.expected_link_target
        );
        let created_id = batch
            .entries
            .iter()
            .find(|entry| entry.target_id == new_target.target_id)
            .map(|entry| DeploymentId::from_str(&entry.deployment_id).unwrap())
            .unwrap();
        assert!(
            !fixture
                .vault
                .repositories
                .deployment(created_id)
                .unwrap()
                .unwrap()
                .active
        );
        assert!(
            !Path::new(
                batch
                    .entries
                    .iter()
                    .find(|entry| entry.target_id == new_target.target_id)
                    .unwrap()
                    .target_path
                    .as_str()
            )
            .exists()
        );
        assert!(
            matches!(fixture.vault.manifests.read_deployment(created_id), Err(ManifestError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
        );
        assert_projected_success(
            &fixture,
            OperationId::from_str(&batch.operation_id).unwrap(),
        );
        let undo_id = OperationId::from_str(&undo.operation_id).unwrap();
        assert_projected_success(&fixture, undo_id);

        fixture
            .vault
            .database
            .execute(move |connection| {
                connection.execute(
                    "DELETE FROM activity WHERE operation_id = ?1",
                    [undo_id.to_string()],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(operation_activity_counts(&fixture, undo_id), (1, 0));
        let activities = ActivityService::new(
            fixture.vault.repositories.clone(),
            OperationStore::open(fixture.vault.paths.manager()).unwrap(),
        );
        activities.rebuild_terminal_operations().unwrap();
        activities.rebuild_terminal_operations().unwrap();
        assert_eq!(operation_activity_counts(&fixture, undo_id), (1, 1));
        let item = activities
            .list(ActivityQuery {
                kind: Some("deployment".into()),
                outcome: Some("succeeded".into()),
                limit: 200,
            })
            .unwrap()
            .into_iter()
            .find(|item| item.operation_id.as_deref() == Some(undo.operation_id.as_str()))
            .unwrap();
        let detail = activities.detail(&item.id).unwrap();
        let operation = detail.operation.unwrap();
        assert_eq!(operation.paths.len(), 2);
        assert!(operation.recovery_available);
        assert!(operation.plan_reference.ends_with("/plan.json"));
        assert!(operation.journal_reference.ends_with("/journal.json"));
    }

    #[test]
    fn undo_postcondition_tamper_refuses_before_persisting_an_inverse() {
        let fixture = fixture();
        let first = target(&fixture, "tamper-first", FixtureTargetKindDto::GitProject);
        let second = target(&fixture, "tamper-second", FixtureTargetKindDto::GitProject);
        let batch = fixture
            .service
            .plan_batch_deployment(&BatchDeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                targets: vec![
                    BatchDeploymentTargetChoice {
                        target_id: first.target_id,
                        requested_mode: Some(DeploymentModeDto::ManagedCopy),
                    },
                    BatchDeploymentTargetChoice {
                        target_id: second.target_id,
                        requested_mode: Some(DeploymentModeDto::ManagedCopy),
                    },
                ],
            })
            .unwrap();
        fixture
            .service
            .execute_any_operation(&batch.operation_id, &batch.plan_digest)
            .unwrap();
        let operations = fixture.vault.paths.manager().join("operations");
        let before = fs::read_dir(&operations).unwrap().count();
        fs::write(
            Path::new(&batch.entries[0].target_path).join("SKILL.md"),
            "tampered\n",
        )
        .unwrap();
        assert!(matches!(
            fixture.service.plan_undo(&batch.operation_id),
            Err(DeploymentError::DriftBlocked(_))
        ));
        assert_eq!(fs::read_dir(&operations).unwrap().count(), before);
    }

    #[test]
    fn three_and_twenty_target_batches_stage_all_then_activate_mixed_modes() {
        for target_count in [3_usize, 20] {
            let fixture = fixture();
            let mut choices = Vec::with_capacity(target_count);
            for index in 0..target_count {
                let (kind, requested_mode) = if index % 2 == 0 {
                    (FixtureTargetKindDto::Global, DeploymentModeDto::Symlink)
                } else {
                    (
                        FixtureTargetKindDto::GitProject,
                        DeploymentModeDto::ManagedCopy,
                    )
                };
                let registered = target(&fixture, &format!("batch-{target_count}-{index}"), kind);
                choices.push(BatchDeploymentTargetChoice {
                    target_id: registered.target_id,
                    requested_mode: Some(requested_mode),
                });
            }

            let reviewed = fixture
                .service
                .plan_batch_deployment(&BatchDeploymentPlanRequest {
                    skill_id: fixture.skill_id.to_string(),
                    targets: choices,
                })
                .unwrap();
            assert_eq!(reviewed.entries.len(), target_count);
            let result = fixture
                .service
                .execute_any_operation(&reviewed.operation_id, &reviewed.plan_digest)
                .unwrap();
            assert!(matches!(result, AnyOperationView::BatchDeployment(_)));

            let working_digest = hash_bundle(&fixture.working, BundleCaps::default())
                .unwrap()
                .digest;
            for entry in &reviewed.entries {
                let path = Path::new(&entry.target_path);
                match entry.resolved_mode {
                    DeploymentModeDto::Symlink => {
                        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
                        assert_eq!(fs::read_link(path).unwrap(), fixture.working);
                    }
                    DeploymentModeDto::ManagedCopy => assert_eq!(
                        hash_bundle(path, BundleCaps::default()).unwrap().digest,
                        working_digest
                    ),
                }
                let deployment_id = DeploymentId::from_str(&entry.deployment_id).unwrap();
                assert!(
                    fixture
                        .vault
                        .repositories
                        .deployment(deployment_id)
                        .unwrap()
                        .is_some_and(|deployment| deployment.active)
                );
                assert!(
                    fixture
                        .vault
                        .manifests
                        .read_deployment(deployment_id)
                        .is_ok()
                );
            }

            let operation_id = OperationId::from_str(&reviewed.operation_id).unwrap();
            let stored = OperationStore::open(fixture.vault.paths.manager())
                .unwrap()
                .load(operation_id)
                .unwrap();
            let last_staged = stored
                .steps
                .iter()
                .filter_map(|step| step.stage.observed_at)
                .max()
                .unwrap();
            let first_commit = stored
                .steps
                .iter()
                .filter_map(|step| step.commit.intent_at)
                .min()
                .unwrap();
            assert!(last_staged <= first_commit);
            assert_eq!(stored.journal.outcome, Some(OperationOutcome::Succeeded));
            assert_projected_success(&fixture, operation_id);
        }
    }

    #[test]
    fn each_batch_target_commit_failure_rolls_back_every_prior_target() {
        for failed_order in 0..3_u32 {
            let fixture = fixture();
            let targets = (0..3)
                .map(|index| {
                    target(
                        &fixture,
                        &format!("rollback-{failed_order}-{index}"),
                        if index % 2 == 0 {
                            FixtureTargetKindDto::Global
                        } else {
                            FixtureTargetKindDto::GitProject
                        },
                    )
                })
                .collect::<Vec<_>>();
            let reviewed = fixture
                .service
                .plan_batch_deployment(&BatchDeploymentPlanRequest {
                    skill_id: fixture.skill_id.to_string(),
                    targets: targets
                        .iter()
                        .enumerate()
                        .map(|(index, target)| BatchDeploymentTargetChoice {
                            target_id: target.target_id.clone(),
                            requested_mode: Some(if index % 2 == 0 {
                                DeploymentModeDto::Symlink
                            } else {
                                DeploymentModeDto::ManagedCopy
                            }),
                        })
                        .collect(),
                })
                .unwrap();
            let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
                Arc::new(FilesystemCapabilityProbe),
                Arc::new(FailAt(OperationBoundary::FinalRenamed(failed_order))),
            );
            assert!(matches!(
                service.execute_any_operation(&reviewed.operation_id, &reviewed.plan_digest),
                Err(DeploymentError::Operation(
                    OperationError::ExecutionFailedRolledBack(_)
                ))
            ));
            for entry in &reviewed.entries {
                assert!(!Path::new(&entry.target_path).exists());
                assert!(
                    fixture
                        .vault
                        .repositories
                        .deployment(DeploymentId::from_str(&entry.deployment_id).unwrap())
                        .unwrap()
                        .is_none()
                );
            }
            let operation_id = OperationId::from_str(&reviewed.operation_id).unwrap();
            let stored = OperationStore::open(fixture.vault.paths.manager())
                .unwrap()
                .load(operation_id)
                .unwrap();
            assert_eq!(
                stored.journal.outcome,
                Some(OperationOutcome::FailedRolledBack)
            );
            assert_eq!(operation_activity_counts(&fixture, operation_id), (1, 1));
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn batch_failpoint_matrix_covers_each_target_durable_boundary_with_exact_rollback() {
        let fixture = fixture();
        let first = target(&fixture, "matrix-first", FixtureTargetKindDto::GitProject);
        let second = target(&fixture, "matrix-second", FixtureTargetKindDto::GitProject);
        let first_initial = plan(&fixture, &first, Some(DeploymentModeDto::ManagedCopy));
        let second_initial = plan(&fixture, &second, Some(DeploymentModeDto::ManagedCopy));
        execute(&fixture, &first_initial);
        execute(&fixture, &second_initial);
        let old_digest = hash_bundle(Path::new(&first_initial.target_path), BundleCaps::default())
            .unwrap()
            .digest;
        let first_before = fixture
            .vault
            .repositories
            .deployment(DeploymentId::from_str(&first_initial.deployment_id).unwrap())
            .unwrap()
            .unwrap();
        let second_before = fixture
            .vault
            .repositories
            .deployment(DeploymentId::from_str(&second_initial.deployment_id).unwrap())
            .unwrap()
            .unwrap();
        advance_working(&fixture, "batch matrix vault ahead\n");

        for order in 0..2_u32 {
            for boundary in [
                OperationBoundary::StageIntentPersisted(order),
                OperationBoundary::StageActionApplied(order),
                OperationBoundary::StageObserved(order),
                OperationBoundary::CommitIntentPersisted(order),
                OperationBoundary::BackupRenamed(order),
                OperationBoundary::FinalRenamed(order),
                OperationBoundary::CommitObserved(order),
                OperationBoundary::VerifyIntentPersisted(order),
                OperationBoundary::VerifyObserved(order),
            ] {
                let reviewed = fixture
                    .service
                    .plan_batch_deployment(&BatchDeploymentPlanRequest {
                        skill_id: fixture.skill_id.to_string(),
                        targets: vec![
                            BatchDeploymentTargetChoice {
                                target_id: first.target_id.clone(),
                                requested_mode: Some(DeploymentModeDto::ManagedCopy),
                            },
                            BatchDeploymentTargetChoice {
                                target_id: second.target_id.clone(),
                                requested_mode: Some(DeploymentModeDto::ManagedCopy),
                            },
                        ],
                    })
                    .unwrap();
                let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
                    Arc::new(FilesystemCapabilityProbe),
                    Arc::new(FailAt(boundary)),
                );
                assert!(
                    service
                        .execute_any_operation(&reviewed.operation_id, &reviewed.plan_digest)
                        .is_err()
                );
                for path in [&first_initial.target_path, &second_initial.target_path] {
                    assert_eq!(
                        hash_bundle(Path::new(path), BundleCaps::default())
                            .unwrap()
                            .digest,
                        old_digest,
                        "boundary {boundary:?} changed an original target"
                    );
                }
                let first_after = fixture
                    .vault
                    .repositories
                    .deployment(first_before.id)
                    .unwrap()
                    .unwrap();
                let second_after = fixture
                    .vault
                    .repositories
                    .deployment(second_before.id)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    first_after.last_operation_id,
                    first_before.last_operation_id
                );
                assert_eq!(
                    second_after.last_operation_id,
                    second_before.last_operation_id
                );

                let operation_id = OperationId::from_str(&reviewed.operation_id).unwrap();
                let stored = OperationStore::open(fixture.vault.paths.manager())
                    .unwrap()
                    .load(operation_id)
                    .unwrap();
                assert!(matches!(
                    stored.journal.outcome,
                    Some(OperationOutcome::FailedNoWrites | OperationOutcome::FailedRolledBack)
                ));
                assert_eq!(operation_activity_counts(&fixture, operation_id), (1, 1));
                assert_eq!(stored.journal.snapshot_protections.len(), 2);
                let snapshot = read_deployment_snapshot(
                    &OperationStore::open(fixture.vault.paths.manager()).unwrap(),
                    operation_id,
                )
                .unwrap();
                assert_eq!(snapshot.protections.len(), 2);
                assert!(
                    snapshot
                        .protections
                        .iter()
                        .all(|protection| protection.reference.starts_with("object:"))
                );
            }
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn batch_finalization_failpoints_reopen_and_finish_idempotently() {
        for boundary in [
            OperationBoundary::ManifestsPublished,
            OperationBoundary::ProjectionFinalized,
        ] {
            let fixture = fixture();
            let link = target(&fixture, "finalize-link", FixtureTargetKindDto::Global);
            let copy = target(&fixture, "finalize-copy", FixtureTargetKindDto::GitProject);
            let reviewed = fixture
                .service
                .plan_batch_deployment(&BatchDeploymentPlanRequest {
                    skill_id: fixture.skill_id.to_string(),
                    targets: vec![
                        BatchDeploymentTargetChoice {
                            target_id: link.target_id,
                            requested_mode: Some(DeploymentModeDto::Symlink),
                        },
                        BatchDeploymentTargetChoice {
                            target_id: copy.target_id,
                            requested_mode: Some(DeploymentModeDto::ManagedCopy),
                        },
                    ],
                })
                .unwrap();
            let interrupted = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
                Arc::new(FilesystemCapabilityProbe),
                Arc::new(FailAt(boundary)),
            );
            assert!(matches!(
                interrupted.execute_any_operation(&reviewed.operation_id, &reviewed.plan_digest),
                Err(DeploymentError::Operation(
                    OperationError::FinalizationInterrupted(_)
                ))
            ));
            let operation_id = OperationId::from_str(&reviewed.operation_id).unwrap();
            let store = OperationStore::open(fixture.vault.paths.manager()).unwrap();
            assert_eq!(
                store.load(operation_id).unwrap().journal.state,
                OperationState::Committed
            );
            let recovered = DeploymentService::new(Arc::clone(&fixture.vault));
            assert_eq!(
                recovered.recover_operation(operation_id).unwrap().outcome,
                OperationOutcome::Succeeded
            );
            for entry in &reviewed.entries {
                let deployment_id = DeploymentId::from_str(&entry.deployment_id).unwrap();
                assert!(
                    fixture
                        .vault
                        .manifests
                        .read_deployment(deployment_id)
                        .is_ok()
                );
                assert!(
                    fixture
                        .vault
                        .repositories
                        .deployment(deployment_id)
                        .unwrap()
                        .is_some_and(|deployment| deployment.active)
                );
            }
            assert_projected_success(&fixture, operation_id);
            let before_replay = reviewed
                .entries
                .iter()
                .map(|entry| {
                    let path = Path::new(&entry.target_path);
                    if fs::symlink_metadata(path).unwrap().file_type().is_symlink() {
                        fs::read_link(path)
                            .unwrap()
                            .into_os_string()
                            .into_encoded_bytes()
                    } else {
                        hash_bundle(path, BundleCaps::default())
                            .unwrap()
                            .digest
                            .bytes()
                            .to_vec()
                    }
                })
                .collect::<Vec<_>>();
            assert!(recovered.recover_operation(operation_id).unwrap().replayed);
            let after_replay = reviewed
                .entries
                .iter()
                .map(|entry| {
                    let path = Path::new(&entry.target_path);
                    if fs::symlink_metadata(path).unwrap().file_type().is_symlink() {
                        fs::read_link(path)
                            .unwrap()
                            .into_os_string()
                            .into_encoded_bytes()
                    } else {
                        hash_bundle(path, BundleCaps::default())
                            .unwrap()
                            .digest
                            .bytes()
                            .to_vec()
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(after_replay, before_replay);
            assert_eq!(operation_activity_counts(&fixture, operation_id), (1, 1));
        }
    }

    impl OperationFailpoints for CrashAt {
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

    #[test]
    fn target_defaults_override_and_proven_fallback_are_sealed_in_new_plans() {
        let fixture = fixture();
        let global = target(&fixture, "global", FixtureTargetKindDto::Global);
        let git = target(&fixture, "git", FixtureTargetKindDto::GitProject);
        let personal = target(&fixture, "personal", FixtureTargetKindDto::PersonalProject);
        assert_eq!(global.default_mode, DeploymentModeDto::Symlink);
        assert_eq!(git.default_mode, DeploymentModeDto::ManagedCopy);
        assert_eq!(personal.default_mode, DeploymentModeDto::Symlink);
        assert_eq!(
            plan(&fixture, &git, None).resolved_mode,
            DeploymentModeDto::ManagedCopy
        );
        assert_eq!(
            plan(&fixture, &global, Some(DeploymentModeDto::ManagedCopy)).resolved_mode,
            DeploymentModeDto::ManagedCopy
        );

        let fallback_service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
            Arc::new(FixedProbe(TargetCapabilityEvidence {
                directory_write: CapabilityStatus::Supported,
                atomic_rename: CapabilityStatus::Supported,
                symlink: CapabilityStatus::Unsupported,
            })),
            Arc::new(crate::operations::NoopOperationFailpoints),
        );
        let fallback = fallback_service
            .plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: global.target_id,
                requested_mode: Some(DeploymentModeDto::Symlink),
            })
            .unwrap();
        assert_eq!(fallback.requested_mode, DeploymentModeDto::Symlink);
        assert_eq!(fallback.resolved_mode, DeploymentModeDto::ManagedCopy);
        assert!(fallback.fallback_reason.is_some());
        assert_ne!(
            fallback.plan_digest,
            plan(&fixture, &personal, None).plan_digest
        );

        let unknown_service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
            Arc::new(FixedProbe(TargetCapabilityEvidence {
                directory_write: CapabilityStatus::Supported,
                atomic_rename: CapabilityStatus::Supported,
                symlink: CapabilityStatus::Unknown,
            })),
            Arc::new(crate::operations::NoopOperationFailpoints),
        );
        assert!(matches!(
            unknown_service.plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: personal.target_id,
                requested_mode: Some(DeploymentModeDto::Symlink),
            }),
            Err(DeploymentError::CapabilityBlocked(_))
        ));
    }

    #[test]
    fn plan_export_is_stable_pretty_json_for_the_persisted_digest() {
        let fixture = fixture();
        let target = target(&fixture, "export-target", FixtureTargetKindDto::Global);
        let planned = plan(&fixture, &target, None);

        let first = fixture
            .service
            .export_plan_json(&planned.operation_id)
            .unwrap();
        let second = fixture
            .service
            .export_plan_json(&planned.operation_id)
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.0, planned.plan_digest);
        assert!(first.1.starts_with("{\n"));
        let exported: serde_json::Value = serde_json::from_str(&first.1).unwrap();
        assert_eq!(
            exported
                .get("planDigest")
                .and_then(serde_json::Value::as_str),
            Some(planned.plan_digest.as_str())
        );
        assert_eq!(
            exported
                .get("operationId")
                .and_then(serde_json::Value::as_str),
            Some(planned.operation_id.as_str())
        );
    }

    #[test]
    fn target_registration_rejects_both_vault_nesting_directions() {
        let fixture = fixture();
        let inside = fixture.vault.paths.root().join("nested-target");
        fs::create_dir(&inside).unwrap();
        assert!(matches!(
            fixture.service.register_target(&RegisterTargetRequest {
                kind: FixtureTargetKindDto::Global,
                selected_directory: inside.to_string_lossy().into_owned(),
                adapter_id: None,
                is_override: None,
            }),
            Err(DeploymentError::InvalidTargetDirectory)
        ));
        assert!(matches!(
            fixture.service.register_target(&RegisterTargetRequest {
                kind: FixtureTargetKindDto::Global,
                selected_directory: fixture.temporary.path().to_string_lossy().into_owned(),
                adapter_id: None,
                is_override: None,
            }),
            Err(DeploymentError::InvalidTargetDirectory)
        ));
        assert!(fixture.service.targets().unwrap().is_empty());
    }

    #[test]
    fn all_six_families_use_the_same_global_git_and_personal_mode_defaults() {
        let fixture = fixture();
        for descriptor in crate::adapters::DESCRIPTORS {
            for (suffix, kind, expected) in [
                (
                    "global",
                    FixtureTargetKindDto::Global,
                    DeploymentModeDto::Symlink,
                ),
                (
                    "git",
                    FixtureTargetKindDto::GitProject,
                    DeploymentModeDto::ManagedCopy,
                ),
                (
                    "personal",
                    FixtureTargetKindDto::PersonalProject,
                    DeploymentModeDto::Symlink,
                ),
            ] {
                let root = fixture
                    .temporary
                    .path()
                    .join(format!("{}-{suffix}", descriptor.name));
                fs::create_dir(&root).unwrap();
                let target = fixture
                    .service
                    .register_target(&RegisterTargetRequest {
                        kind,
                        selected_directory: root.to_string_lossy().into_owned(),
                        adapter_id: Some(descriptor.id().to_string()),
                        is_override: None,
                    })
                    .unwrap();
                assert_eq!(target.default_mode, expected);
                let reviewed = plan(&fixture, &target, None);
                assert_eq!(
                    reviewed.resolved_mode, expected,
                    "{} {suffix}",
                    descriptor.name
                );
                execute(&fixture, &reviewed);
                assert_eq!(
                    fixture
                        .service
                        .verify(&reviewed.deployment_id)
                        .unwrap()
                        .health,
                    "clean",
                    "{} {suffix}",
                    descriptor.name
                );
            }
        }
    }

    #[test]
    fn custom_target_replacement_blocks_planning_until_explicit_reselection() {
        let fixture = fixture();
        let root = fixture.temporary.path().join("custom-target");
        fs::create_dir(&root).unwrap();
        let mut request = CustomTargetRegisterRequest {
            target_id: None,
            display_name: "Custom project Skills".into(),
            selected_directory: root.to_string_lossy().into_owned(),
            scope: CustomTargetScope::Project,
            preferred_mode: DeploymentModeDto::ManagedCopy,
            project_id: None,
        };
        let target = fixture.service.register_custom_target(&request).unwrap();
        assert_eq!(target.default_mode, DeploymentModeDto::ManagedCopy);
        assert_eq!(
            plan(&fixture, &target, None).resolved_mode,
            DeploymentModeDto::ManagedCopy
        );

        fs::remove_dir(&root).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(matches!(
            fixture.service.plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: target.target_id.clone(),
                requested_mode: None,
            }),
            Err(DeploymentError::TargetMissing)
        ));
        assert!(matches!(
            fixture.service.register_custom_target(&request),
            Err(DeploymentError::TargetMissing)
        ));

        request.target_id = Some(target.target_id.clone());
        let reselected = fixture.service.register_custom_target(&request).unwrap();
        assert_eq!(reselected.target_id, target.target_id);
        let reviewed = plan(&fixture, &reselected, None);
        execute(&fixture, &reviewed);
        assert!(Path::new(&reviewed.target_path).is_dir());
    }

    #[test]
    fn adapter_disable_and_override_change_revoke_reviewed_target_authority() {
        let fixture = fixture();
        let first = fixture.temporary.path().join("configured-first");
        let second = fixture.temporary.path().join("configured-second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let adapter_id = crate::adapters::DESCRIPTORS[0].id().to_string();
        fixture
            .service
            .configure_adapter(&AdapterConfigureRequest {
                adapter_id: adapter_id.clone(),
                enabled: true,
                global_override_path: Some(first.to_string_lossy().into_owned()),
                project_override_path: None,
            })
            .unwrap();
        let stale = fixture
            .service
            .targets()
            .unwrap()
            .into_iter()
            .find(|target| target.root_path == first.to_string_lossy())
            .unwrap();
        let reviewed = plan(&fixture, &stale, None);
        fixture
            .service
            .configure_adapter(&AdapterConfigureRequest {
                adapter_id: adapter_id.clone(),
                enabled: true,
                global_override_path: Some(second.to_string_lossy().into_owned()),
                project_override_path: None,
            })
            .unwrap();
        assert!(matches!(
            fixture
                .service
                .execute_any_operation(&reviewed.operation_id, &reviewed.plan_digest),
            Err(DeploymentError::TargetMissing)
        ));
        fixture
            .service
            .configure_adapter(&AdapterConfigureRequest {
                adapter_id,
                enabled: false,
                global_override_path: Some(second.to_string_lossy().into_owned()),
                project_override_path: None,
            })
            .unwrap();
        assert!(matches!(
            fixture.service.plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: stale.target_id,
                requested_mode: None,
            }),
            Err(DeploymentError::TargetMissing)
        ));
    }

    #[test]
    fn configured_project_override_materializes_a_contained_adapter_target() {
        let fixture = fixture();
        let root = fixture.temporary.path().join("configured-project");
        let target_root = root.join(".custom/skills");
        fs::create_dir_all(&target_root).unwrap();
        let root = root.canonicalize().unwrap();
        let project_id = ProjectId::generate();
        let now = UtcTimestamp::now();
        fixture
            .vault
            .repositories
            .upsert_project(ProjectRecord {
                id: project_id,
                workspace_root_id: None,
                root_path: root.clone(),
                canonical_path: root,
                discovery_evidence: "test".into(),
                git_classification: "git".into(),
                manual: true,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let adapter_id = crate::adapters::DESCRIPTORS[1].id().to_string();
        fixture
            .service
            .configure_adapter(&AdapterConfigureRequest {
                adapter_id: adapter_id.clone(),
                enabled: true,
                global_override_path: None,
                project_override_path: Some(".custom/skills".into()),
            })
            .unwrap();
        let target = fixture
            .service
            .register_adapter_project_target(&AdapterProjectTargetRegisterRequest {
                adapter_id,
                project_id: project_id.to_string(),
            })
            .unwrap();
        assert_eq!(
            Path::new(&target.root_path),
            target_root.canonicalize().unwrap()
        );
        assert!(target.is_override);
        assert_eq!(target.default_mode, DeploymentModeDto::ManagedCopy);
        assert!(
            fixture
                .service
                .plan_deployment(&DeploymentPlanRequest {
                    skill_id: fixture.skill_id.to_string(),
                    target_id: target.target_id,
                    requested_mode: None,
                })
                .is_ok()
        );
    }

    #[test]
    fn adapter_version_change_marks_deployment_unverified_without_rewriting_path() {
        let fixture = fixture();
        let target = target(&fixture, "versioned", FixtureTargetKindDto::GitProject);
        let reviewed = plan(&fixture, &target, None);
        execute(&fixture, &reviewed);
        let deployment_id = DeploymentId::from_str(&reviewed.deployment_id).unwrap();
        let before = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        let mut bumped_target = fixture
            .vault
            .repositories
            .target(TargetId::from_str(&target.target_id).unwrap())
            .unwrap()
            .unwrap();
        bumped_target.adapter_id = AdapterId::new("universal-agent-skills", 2).unwrap();
        fixture
            .vault
            .repositories
            .upsert_target(bumped_target)
            .unwrap();

        let health = fixture.service.verify(&reviewed.deployment_id).unwrap();
        let after = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert_eq!(health.health, "unverified");
        assert_eq!(after.target_path, before.target_path);
        assert_eq!(after.adapter_version, before.adapter_version);
    }

    #[test]
    fn absent_copy_and_symlink_deploy_verify_manifest_projection_and_replay() {
        let fixture = fixture();
        let copy_target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let link_target = target(&fixture, "link", FixtureTargetKindDto::Global);
        let copy = plan(&fixture, &copy_target, None);
        let copy_result = execute(&fixture, &copy);
        assert_eq!(copy_result.outcome.as_deref(), Some("Succeeded"));
        assert_eq!(
            hash_bundle(Path::new(&copy.target_path), BundleCaps::default())
                .unwrap()
                .digest,
            hash_bundle(&fixture.working, BundleCaps::default())
                .unwrap()
                .digest
        );
        let link = plan(&fixture, &link_target, None);
        execute(&fixture, &link);
        assert_eq!(fs::read_link(&link.target_path).unwrap(), fixture.working);
        let deployment_id = DeploymentId::from_str(&link.deployment_id).unwrap();
        let record = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert!(record.active);
        assert_eq!(record.health, DeploymentHealth::Clean);
        assert!(
            fixture
                .vault
                .manifests
                .read_deployment(deployment_id)
                .is_ok()
        );
        let replay = execute(&fixture, &link);
        assert!(replay.replayed);
        assert_eq!(fs::read_link(&link.target_path).unwrap(), fixture.working);
    }

    /*
    fn symlink_vault_ahead_is_live_and_explicit_no_write_reverification_advances_expected() {
        let fixture = fixture();
        let target = target(&fixture, "link", FixtureTargetKindDto::Global);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let before = MetadataFingerprint::from_metadata(
            &fs::symlink_metadata(&deployed.target_path).unwrap(),
        );
        let digest = advance_working(&fixture, "live through link\n");
        let health = fixture.service.verify(&deployed.deployment_id).unwrap();
        assert_eq!(health.health, "vault_ahead");
        assert!(health.explanation.contains("already live"));
        let reviewed = plan(&fixture, &target, None);
        assert!(reviewed.no_op);
        execute(&fixture, &reviewed);
        let after = MetadataFingerprint::from_metadata(
            &fs::symlink_metadata(&deployed.target_path).unwrap(),
        );
        assert_eq!((after.device_id, after.file_id), (before.device_id, before.file_id));
        assert_eq!(fixture.service.verify(&deployed.deployment_id).unwrap().health, "clean");
        assert_eq!(
            fixture
                .vault
                .repositories
                .deployment(DeploymentId::from_str(&deployed.deployment_id).unwrap())
                .unwrap()
                .unwrap()
                .expected_digest,
            digest
        );
    }
    */
    #[test]
    fn symlink_vault_ahead_reverification_updates_expected_digest() {
        let fixture = fixture();
        let target = target(&fixture, "link", FixtureTargetKindDto::Global);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let before = MetadataFingerprint::from_metadata(
            &fs::symlink_metadata(&deployed.target_path).unwrap(),
        );
        let digest = advance_working(&fixture, "live through link\n");
        let health = fixture.service.verify(&deployed.deployment_id).unwrap();
        assert_eq!(health.health, "vault_ahead");
        assert!(health.explanation.contains("already live"));
        let reviewed = plan(&fixture, &target, None);
        assert!(reviewed.no_op);
        execute(&fixture, &reviewed);
        let after = MetadataFingerprint::from_metadata(
            &fs::symlink_metadata(&deployed.target_path).unwrap(),
        );
        assert_eq!(
            (after.device_id, after.file_id),
            (before.device_id, before.file_id)
        );
        assert_eq!(
            fixture
                .service
                .verify(&deployed.deployment_id)
                .unwrap()
                .health,
            "clean"
        );
        assert_eq!(
            fixture
                .vault
                .repositories
                .deployment(DeploymentId::from_str(&deployed.deployment_id).unwrap())
                .unwrap()
                .unwrap()
                .expected_digest,
            digest
        );
    }

    #[test]
    fn deployment_finalizer_is_idempotent_for_manifest_projection_and_activity() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let reviewed = plan(&fixture, &target, None);
        execute(&fixture, &reviewed);
        let operation_id = OperationId::from_str(&reviewed.operation_id).unwrap();
        let store = OperationStore::open(fixture.vault.paths.manager()).unwrap();
        let stored = store.load(operation_id).unwrap();
        let hooks = DeploymentHooks {
            vault: Arc::clone(&fixture.vault),
            store,
            capability_probe: Arc::new(FilesystemCapabilityProbe),
        };
        hooks
            .publish_manifests(&stored.plan, &stored.journal)
            .unwrap();
        hooks
            .finalize_projection(&stored.plan, &stored.journal)
            .unwrap();
        hooks
            .publish_manifests(&stored.plan, &stored.journal)
            .unwrap();
        hooks
            .finalize_projection(&stored.plan, &stored.journal)
            .unwrap();
        assert_eq!(
            fixture
                .vault
                .repositories
                .deployments(None, None, true, 500)
                .unwrap()
                .len(),
            1
        );
        let activity_count: i64 = fixture
            .vault
            .database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT count(*) FROM activity WHERE operation_id = ?1",
                        [operation_id.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(crate::persistence::DbExecutorError::Sqlite)
            })
            .unwrap();
        assert_eq!(activity_count, 1);
    }

    #[test]
    fn collision_is_no_write_and_clean_redeploy_does_not_replace_target() {
        let fixture = fixture();
        let collision_target = target(&fixture, "collision", FixtureTargetKindDto::Global);
        let collision = Path::new(&collision_target.root_path).join("example");
        fs::create_dir(&collision).unwrap();
        fs::write(collision.join("foreign"), "keep").unwrap();
        assert!(matches!(
            fixture.service.plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: collision_target.target_id,
                requested_mode: None,
            }),
            Err(DeploymentError::UnmanagedCollision)
        ));
        assert_eq!(
            fs::read_to_string(collision.join("foreign")).unwrap(),
            "keep"
        );

        let folded_target = target(&fixture, "folded", FixtureTargetKindDto::Global);
        let folded = Path::new(&folded_target.root_path).join("Example");
        fs::create_dir(&folded).unwrap();
        assert!(matches!(
            fixture.service.plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: folded_target.target_id,
                requested_mode: None,
            }),
            Err(DeploymentError::UnmanagedCollision)
        ));

        let raced_target = target(&fixture, "raced", FixtureTargetKindDto::Global);
        let raced_plan = plan(&fixture, &raced_target, None);
        let raced_collision = Path::new(&raced_target.root_path).join("EXAMPLE");
        fs::create_dir(&raced_collision).unwrap();
        fs::write(raced_collision.join("foreign"), "preserve").unwrap();
        assert!(matches!(
            fixture
                .service
                .execute_operation(&raced_plan.operation_id, &raced_plan.plan_digest),
            Err(DeploymentError::Operation(
                OperationError::StageFailed(_) | OperationError::StalePlan { .. }
            ))
        ));
        assert_eq!(
            fs::read_to_string(raced_collision.join("foreign")).unwrap(),
            "preserve"
        );

        let managed = target(&fixture, "managed", FixtureTargetKindDto::GitProject);
        let first = plan(&fixture, &managed, None);
        execute(&fixture, &first);
        let before =
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&first.target_path).unwrap());
        let no_op = plan(&fixture, &managed, None);
        assert!(no_op.no_op);
        execute(&fixture, &no_op);
        let after =
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&first.target_path).unwrap());
        assert_eq!(
            (after.device_id, after.file_id),
            (before.device_id, before.file_id)
        );
    }

    #[test]
    fn managed_copy_vault_ahead_redeploys_with_snapshot_and_drift_blocks_overwrite() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let first = plan(&fixture, &target, None);
        execute(&fixture, &first);
        let old_digest = hash_bundle(Path::new(&first.target_path), BundleCaps::default())
            .unwrap()
            .digest;
        let new_digest = advance_working(&fixture, "vault ahead\n");
        assert_eq!(
            fixture.service.verify(&first.deployment_id).unwrap().health,
            "vault_ahead"
        );
        let redeploy = plan(&fixture, &target, None);
        assert_eq!(redeploy.recovery_count, 1);
        execute(&fixture, &redeploy);
        assert_eq!(
            hash_bundle(Path::new(&first.target_path), BundleCaps::default())
                .unwrap()
                .digest,
            new_digest
        );
        let operation_id = OperationId::from_str(&redeploy.operation_id).unwrap();
        let snapshot = read_deployment_snapshot(
            &OperationStore::open(fixture.vault.paths.manager()).unwrap(),
            operation_id,
        )
        .unwrap();
        assert_eq!(
            snapshot.protections[0].before.bundle_digest,
            Some(old_digest)
        );

        fs::write(
            Path::new(&first.target_path).join("SKILL.md"),
            "target edit\n",
        )
        .unwrap();
        assert_eq!(
            fixture.service.verify(&first.deployment_id).unwrap().health,
            "target_modified"
        );
        assert!(matches!(
            fixture.service.plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: target.target_id,
                requested_mode: None,
            }),
            Err(DeploymentError::DriftBlocked(_))
        ));
    }

    #[test]
    fn changed_deployment_authority_stops_redeploy_before_active_write() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let before = hash_bundle(Path::new(&deployed.target_path), BundleCaps::default()).unwrap();
        advance_working(&fixture, "vault ahead\n");
        let redeploy = plan(&fixture, &target, None);
        let deployment_id = DeploymentId::from_str(&deployed.deployment_id).unwrap();
        let mut record = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        thread::sleep(Duration::from_millis(2));
        record.updated_at = UtcTimestamp::now();
        fixture
            .vault
            .repositories
            .upsert_deployment(record)
            .unwrap();
        assert!(matches!(
            fixture
                .service
                .execute_operation(&redeploy.operation_id, &redeploy.plan_digest),
            Err(DeploymentError::Operation(OperationError::StageFailed(_)))
        ));
        assert_eq!(
            hash_bundle(Path::new(&deployed.target_path), BundleCaps::default()).unwrap(),
            before
        );
    }

    #[test]
    fn verification_reads_do_not_invalidate_a_reviewed_redeploy_plan() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let expected = advance_working(&fixture, "vault ahead\n");
        let redeploy = plan(&fixture, &target, None);
        assert_eq!(
            fixture
                .service
                .verify(&deployed.deployment_id)
                .unwrap()
                .health,
            "vault_ahead"
        );
        assert_eq!(
            fixture
                .service
                .deployments_list(&DeploymentQuery {
                    skill_id: Some(fixture.skill_id.to_string()),
                    target_id: Some(target.target_id),
                    include_inactive: false,
                    limit: 20,
                })
                .unwrap()
                .count,
            1
        );
        execute(&fixture, &redeploy);
        assert_eq!(
            hash_bundle(Path::new(&deployed.target_path), BundleCaps::default())
                .unwrap()
                .digest,
            expected
        );
    }

    #[test]
    fn capability_change_before_commit_never_silently_switches_mode() {
        let fixture = fixture();
        let target = target(&fixture, "link", FixtureTargetKindDto::Global);
        let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
            Arc::new(ChangeProbe {
                calls: AtomicUsize::new(0),
                supported_calls: 2,
            }),
            Arc::new(crate::operations::NoopOperationFailpoints),
        );
        let plan = service
            .plan_deployment(&DeploymentPlanRequest {
                skill_id: fixture.skill_id.to_string(),
                target_id: target.target_id,
                requested_mode: Some(DeploymentModeDto::Symlink),
            })
            .unwrap();
        assert!(matches!(
            service.execute_operation(&plan.operation_id, &plan.plan_digest),
            Err(DeploymentError::Operation(OperationError::StalePlan { .. }))
        ));
        assert!(!Path::new(&plan.target_path).exists());
        let stored = OperationStore::open(fixture.vault.paths.manager())
            .unwrap()
            .load(OperationId::from_str(&plan.operation_id).unwrap())
            .unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
    }

    fn write_bundle(path: &Path, body: &str) -> BundleDigest {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(path).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
        }
        fs::create_dir(path).unwrap();
        fs::write(path.join("SKILL.md"), body).unwrap();
        hash_bundle(path, BundleCaps::default()).unwrap().digest
    }

    fn health_record(
        target_path: PathBuf,
        mode: DeploymentMode,
        expected: BundleDigest,
        link: Option<PathBuf>,
    ) -> DeploymentRecord {
        let now = UtcTimestamp::now();
        DeploymentRecord {
            id: DeploymentId::generate(),
            skill_id: SkillId::generate(),
            target_id: TargetId::generate(),
            deployment_name: DeploymentName::parse("example").unwrap(),
            target_path,
            mode,
            expected_digest: expected,
            expected_link_target: link,
            health: DeploymentHealth::Unverified,
            adapter_version: adapter_id().unwrap(),
            active: true,
            last_verified_at: None,
            last_operation_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn managed_copy_and_symlink_health_follow_the_full_truth_tables() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault-working");
        let target = temporary.path().join("target");
        let expected = write_bundle(&vault, "expected\n");
        write_bundle(&target, "expected\n");
        let copy = health_record(target.clone(), DeploymentMode::ManagedCopy, expected, None);
        assert_eq!(
            evaluate_target(&copy, Some(expected)).health,
            DeploymentHealth::Clean
        );
        let vault_ahead = write_bundle(&vault, "vault ahead\n");
        assert_eq!(
            evaluate_target(&copy, Some(vault_ahead)).health,
            DeploymentHealth::VaultAhead
        );
        write_bundle(&vault, "expected\n");
        let target_modified = write_bundle(&target, "target modified\n");
        assert_eq!(
            evaluate_target(&copy, Some(expected)).health,
            DeploymentHealth::TargetModified
        );
        let vault_conflict = write_bundle(&vault, "vault conflict\n");
        assert_ne!(vault_conflict, target_modified);
        assert_eq!(
            evaluate_target(&copy, Some(vault_conflict)).health,
            DeploymentHealth::Conflict
        );
        let same_new = write_bundle(&vault, "same new\n");
        assert_eq!(write_bundle(&target, "same new\n"), same_new);
        assert_eq!(
            evaluate_target(&copy, Some(same_new)).health,
            DeploymentHealth::Unverified
        );
        fs::remove_dir_all(&target).unwrap();
        assert_eq!(
            evaluate_target(&copy, Some(same_new)).health,
            DeploymentHealth::MissingTarget
        );
        fs::write(&target, "wrong entry").unwrap();
        assert_eq!(
            evaluate_target(&copy, Some(same_new)).health,
            DeploymentHealth::Conflict
        );
        fs::remove_file(&target).unwrap();
        fs::create_dir(&target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing", target.join("broken")).unwrap();
        assert_eq!(
            evaluate_target(&copy, Some(same_new)).health,
            DeploymentHealth::Unverified
        );

        fs::remove_dir_all(&target).unwrap();
        let expected = write_bundle(&vault, "link expected\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&vault, &target).unwrap();
        let link = health_record(
            target.clone(),
            DeploymentMode::Symlink,
            expected,
            Some(vault.clone()),
        );
        assert_eq!(
            evaluate_target(&link, Some(expected)).health,
            DeploymentHealth::Clean
        );
        let ahead = write_bundle(&vault, "link live change\n");
        assert_eq!(
            evaluate_target(&link, Some(ahead)).health,
            DeploymentHealth::VaultAhead
        );
        fs::remove_file(&target).unwrap();
        assert_eq!(
            evaluate_target(&link, Some(ahead)).health,
            DeploymentHealth::MissingTarget
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(temporary.path().join("missing-vault"), &target).unwrap();
        let broken = health_record(
            target.clone(),
            DeploymentMode::Symlink,
            expected,
            Some(temporary.path().join("missing-vault")),
        );
        assert_eq!(
            evaluate_target(&broken, Some(ahead)).health,
            DeploymentHealth::BrokenLink
        );
        fs::remove_file(&target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(temporary.path(), &target).unwrap();
        assert_eq!(
            evaluate_target(&link, Some(ahead)).health,
            DeploymentHealth::Conflict
        );
        fs::remove_file(&target).unwrap();
        fs::create_dir(&target).unwrap();
        assert_eq!(
            evaluate_target(&link, Some(ahead)).health,
            DeploymentHealth::Conflict
        );
        assert_eq!(
            evaluate_target(&link, None).health,
            DeploymentHealth::Conflict,
            "entry conflicts take precedence over unavailable Vault evidence"
        );
    }

    #[test]
    fn undeploys_exact_copy_and_link_with_snapshots_and_one_target_isolation() {
        let fixture = fixture();
        let copy_target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let link_target = target(&fixture, "link", FixtureTargetKindDto::Global);
        let copy = plan(&fixture, &copy_target, None);
        let link = plan(&fixture, &link_target, None);
        execute(&fixture, &copy);
        execute(&fixture, &link);
        let copy_undeploy = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: copy.deployment_id.clone(),
                resolution: UndeployResolutionDto::RemoveManaged,
            })
            .unwrap();
        execute(&fixture, &copy_undeploy);
        assert!(!Path::new(&copy.target_path).exists());
        assert_eq!(fs::read_link(&link.target_path).unwrap(), fixture.working);
        let copy_record = fixture
            .vault
            .repositories
            .deployment(DeploymentId::from_str(&copy.deployment_id).unwrap())
            .unwrap()
            .unwrap();
        assert!(!copy_record.active);
        let copy_snapshot = read_deployment_snapshot(
            &OperationStore::open(fixture.vault.paths.manager()).unwrap(),
            OperationId::from_str(&copy_undeploy.operation_id).unwrap(),
        )
        .unwrap();
        assert!(
            copy_snapshot.protections[0]
                .reference
                .starts_with("object:")
        );
        let takeover = TakeoverService::new(Arc::clone(&fixture.vault));
        assert_eq!(
            takeover
                .skill_detail(&fixture.skill_id.to_string())
                .unwrap()
                .ownership,
            "managed"
        );

        let link_undeploy = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: link.deployment_id.clone(),
                resolution: UndeployResolutionDto::RemoveManaged,
            })
            .unwrap();
        let first = execute(&fixture, &link_undeploy);
        assert!(!Path::new(&link.target_path).exists());
        let link_snapshot = read_deployment_snapshot(
            &OperationStore::open(fixture.vault.paths.manager()).unwrap(),
            OperationId::from_str(&link_undeploy.operation_id).unwrap(),
        )
        .unwrap();
        assert!(link_snapshot.protections[0].reference.starts_with("link:"));
        assert_eq!(
            takeover
                .skill_detail(&fixture.skill_id.to_string())
                .unwrap()
                .ownership,
            "vaulted"
        );
        assert!(execute(&fixture, &link_undeploy).replayed);
        assert_eq!(first.outcome.as_deref(), Some("Succeeded"));
    }

    #[test]
    fn changed_target_requires_preserve_and_preserve_never_deletes_bytes() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        fs::write(
            Path::new(&deployed.target_path).join("SKILL.md"),
            "locally changed\n",
        )
        .unwrap();
        assert!(matches!(
            fixture.service.plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id.clone(),
                resolution: UndeployResolutionDto::RemoveManaged,
            }),
            Err(DeploymentError::DriftBlocked(_))
        ));
        let preserve = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id.clone(),
                resolution: UndeployResolutionDto::PreserveTarget,
            })
            .unwrap();
        assert!(preserve.no_op);
        execute(&fixture, &preserve);
        assert_eq!(
            fs::read_to_string(Path::new(&deployed.target_path).join("SKILL.md")).unwrap(),
            "locally changed\n"
        );
        assert!(
            !fixture
                .vault
                .repositories
                .deployment(DeploymentId::from_str(&deployed.deployment_id).unwrap())
                .unwrap()
                .unwrap()
                .active
        );
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_lines)]
    fn assert_retargeted_entry_can_be_preserved(
        kind: FixtureTargetKindDto,
        alternate_exists: bool,
    ) {
        let fixture = fixture();
        let target = target(&fixture, "retarget", kind);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let target_path = PathBuf::from(&deployed.target_path);
        let alternate = fixture.temporary.path().join(if alternate_exists {
            "user-retarget"
        } else {
            "missing-user-retarget"
        });
        let alternate_before = alternate_exists.then(|| {
            fs::create_dir(&alternate).unwrap();
            fs::write(alternate.join("SKILL.md"), b"user-owned exact bytes\n").unwrap();
            fs::write(alternate.join("notes.txt"), b"do not inspect or change\n").unwrap();
            hash_bundle(&alternate, BundleCaps::default()).unwrap()
        });
        let target_metadata = fs::symlink_metadata(&target_path).unwrap();
        if target_metadata.file_type().is_symlink() {
            fs::remove_file(&target_path).unwrap();
        } else {
            fs::remove_dir_all(&target_path).unwrap();
        }
        std::os::unix::fs::symlink(&alternate, &target_path).unwrap();
        let reviewed_link =
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap());
        let vault_before = hash_bundle(&fixture.working, BundleCaps::default()).unwrap();
        let health = fixture.service.verify(&deployed.deployment_id).unwrap();
        assert_eq!(health.health, "conflict");
        assert!(
            health
                .allowed_actions
                .contains(&"undeploy_preserve".to_owned())
        );

        let preserve = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id.clone(),
                resolution: UndeployResolutionDto::PreserveTarget,
            })
            .unwrap();
        let operation_id = OperationId::from_str(&preserve.operation_id).unwrap();
        let store = OperationStore::open(fixture.vault.paths.manager()).unwrap();
        let planned = store.load(operation_id).unwrap();
        assert_eq!(
            planned.plan.content.steps[0].before.expected_kind,
            EntryKind::Symlink
        );
        assert_eq!(
            planned.plan.content.steps[0].before.raw_symlink_target,
            Some(alternate.to_string_lossy().into_owned())
        );
        assert_eq!(
            planned.plan.content.steps[0].before.metadata,
            Some(reviewed_link)
        );
        assert_eq!(
            planned.plan.content.steps[0].before.resolved_bundle_digest, None,
            "retargeted links must never cause the executor to hash their destination"
        );

        let completed = execute(&fixture, &preserve);
        assert_eq!(completed.outcome.as_deref(), Some("Succeeded"));
        assert_eq!(fs::read_link(&target_path).unwrap(), alternate);
        assert_eq!(
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap()),
            reviewed_link
        );
        assert_eq!(
            hash_bundle(&fixture.working, BundleCaps::default()).unwrap(),
            vault_before
        );
        if let Some(alternate_before) = alternate_before {
            assert_eq!(
                hash_bundle(&alternate, BundleCaps::default()).unwrap(),
                alternate_before
            );
            assert_eq!(
                fs::read(alternate.join("SKILL.md")).unwrap(),
                b"user-owned exact bytes\n"
            );
            assert_eq!(
                fs::read(alternate.join("notes.txt")).unwrap(),
                b"do not inspect or change\n"
            );
        } else {
            assert!(!alternate.exists());
            assert!(fs::metadata(&target_path).is_err());
        }
        let deployment_id = DeploymentId::from_str(&deployed.deployment_id).unwrap();
        let record = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert!(!record.active);
        assert_eq!(record.last_operation_id, Some(operation_id));
        assert!(
            !fixture
                .vault
                .manifests
                .deployment_path(deployment_id)
                .exists()
        );
        let stored = store.load(operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Finalized);
        assert_eq!(stored.journal.outcome, Some(OperationOutcome::Succeeded));
        assert_projected_success(&fixture, operation_id);
    }

    #[test]
    #[cfg(unix)]
    fn retargeted_managed_symlink_preserves_link_and_user_destination_without_reading_it() {
        assert_retargeted_entry_can_be_preserved(FixtureTargetKindDto::Global, true);
    }

    #[test]
    #[cfg(unix)]
    fn managed_copy_replaced_by_symlink_can_be_preserved_without_touching_destination() {
        assert_retargeted_entry_can_be_preserved(FixtureTargetKindDto::GitProject, true);
    }

    #[test]
    #[cfg(unix)]
    fn dangling_retarget_is_preserved_without_resolving_its_missing_destination() {
        assert_retargeted_entry_can_be_preserved(FixtureTargetKindDto::Global, false);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_raw_link_target_blocks_preserve_planning_without_persisting_lossy_evidence() {
        let fixture = fixture();
        let target = target(&fixture, "non-utf8", FixtureTargetKindDto::Global);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let deployment_id = DeploymentId::from_str(&deployed.deployment_id).unwrap();
        let target_path = PathBuf::from(&deployed.target_path);
        fs::remove_file(&target_path).unwrap();
        let raw_target = PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'm', b'i', b's', b's', b'i', b'n', b'g', b'-', 0xff,
        ]));
        std::os::unix::fs::symlink(&raw_target, &target_path).unwrap();
        let raw_before = fs::read_link(&target_path).unwrap();
        let metadata_before =
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap());
        let manifest_path = fixture.vault.manifests.deployment_path(deployment_id);
        let manifest_before = fs::read(&manifest_path).unwrap();
        let health = fixture.service.verify(&deployed.deployment_id).unwrap();
        assert_eq!(health.health, "conflict");
        let record_before = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        let projection_before = projection_totals(&fixture);
        let operations_root = OperationStore::open(fixture.vault.paths.manager())
            .unwrap()
            .operations_root()
            .to_path_buf();
        let operations_before = fs::read_dir(&operations_root).unwrap().count();

        let result = fixture.service.plan_undeploy(&UndeployPlanRequest {
            deployment_id: deployed.deployment_id,
            resolution: UndeployResolutionDto::PreserveTarget,
        });
        assert!(matches!(
            result,
            Err(DeploymentError::DriftBlocked(detail))
                if detail == "current raw symlink target is not valid UTF-8, so exact Operation evidence cannot be sealed"
        ));
        assert_eq!(fs::read_link(&target_path).unwrap(), raw_before);
        assert_eq!(
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap()),
            metadata_before
        );
        assert!(!target_path.parent().unwrap().join(&raw_target).exists());
        assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
        let record_after = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert!(record_after.active);
        assert_eq!(record_after.health, DeploymentHealth::Conflict);
        assert_eq!(
            record_after.last_operation_id,
            record_before.last_operation_id
        );
        assert_eq!(record_after.expected_digest, record_before.expected_digest);
        assert_eq!(record_after.updated_at, record_before.updated_at);
        assert_eq!(projection_totals(&fixture), projection_before);
        assert_eq!(
            fs::read_dir(&operations_root).unwrap().count(),
            operations_before
        );
    }

    #[test]
    #[cfg(unix)]
    #[allow(clippy::too_many_lines)]
    fn broken_link_read_model_preserve_finishes_relationship_without_following_link() {
        let fixture = fixture();
        let target = target(&fixture, "broken", FixtureTargetKindDto::Global);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let deployment_id = DeploymentId::from_str(&deployed.deployment_id).unwrap();
        let target_path = PathBuf::from(&deployed.target_path);
        let missing_target = fixture.temporary.path().join("missing-link-destination");
        let mut record = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        record.expected_link_target = Some(missing_target.clone());
        record.health = DeploymentHealth::Unverified;
        fixture
            .vault
            .repositories
            .upsert_deployment(record)
            .unwrap();
        let mut manifest = fixture
            .vault
            .manifests
            .read_deployment(deployment_id)
            .unwrap();
        manifest.expected_link_target = Some(missing_target.clone());
        fixture.vault.manifests.write_deployment(&manifest).unwrap();
        fs::remove_file(&target_path).unwrap();
        std::os::unix::fs::symlink(&missing_target, &target_path).unwrap();
        let link_before = fs::read_link(&target_path).unwrap();
        let metadata_before =
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap());
        let vault_before = hash_bundle(&fixture.working, BundleCaps::default()).unwrap();
        assert!(!missing_target.exists());

        let health = fixture.service.verify(&deployed.deployment_id).unwrap();
        assert_eq!(health.health, "broken_link");
        assert_eq!(
            health.actual_link_target.as_deref(),
            missing_target.to_str()
        );
        assert!(
            health
                .allowed_actions
                .contains(&"undeploy_preserve".to_owned())
        );
        assert!(
            health
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("preserve can end"))
        );
        let preserve = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id,
                resolution: UndeployResolutionDto::PreserveTarget,
            })
            .unwrap();
        let operation_id = OperationId::from_str(&preserve.operation_id).unwrap();
        let store = OperationStore::open(fixture.vault.paths.manager()).unwrap();
        let planned = store.load(operation_id).unwrap();
        assert_eq!(preserve.reviewed_health, "broken_link");
        assert_eq!(
            planned.plan.content.steps[0].action,
            PlanAction::LeaveUntouched
        );
        assert_eq!(
            planned.plan.content.steps[0]
                .before
                .raw_symlink_target
                .as_deref(),
            missing_target.to_str()
        );
        assert_eq!(
            planned.plan.content.steps[0].before.resolved_bundle_digest,
            None
        );

        execute(&fixture, &preserve);
        assert_eq!(fs::read_link(&target_path).unwrap(), link_before);
        assert_eq!(
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap()),
            metadata_before
        );
        assert!(!missing_target.exists());
        assert_eq!(
            hash_bundle(&fixture.working, BundleCaps::default()).unwrap(),
            vault_before
        );
        let record = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert!(!record.active);
        assert_eq!(record.last_operation_id, Some(operation_id));
        assert!(
            !fixture
                .vault
                .manifests
                .deployment_path(deployment_id)
                .exists()
        );
        let stored = store.load(operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Finalized);
        assert_eq!(stored.journal.outcome, Some(OperationOutcome::Succeeded));
        assert_projected_success(&fixture, operation_id);
    }

    #[test]
    fn missing_target_has_typed_no_write_refusal_and_safe_preserve_resolution() {
        let fixture = fixture();
        let target = target(&fixture, "missing", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let deployment_id = DeploymentId::from_str(&deployed.deployment_id).unwrap();
        let target_path = PathBuf::from(&deployed.target_path);
        fs::remove_dir_all(&target_path).unwrap();
        let health = fixture.service.verify(&deployed.deployment_id).unwrap();
        assert_eq!(health.health, "missing_target");
        assert!(
            health
                .allowed_actions
                .contains(&"undeploy_preserve".to_owned())
        );
        assert_eq!(health.disabled_reason, None);

        let manifest_path = fixture.vault.manifests.deployment_path(deployment_id);
        let manifest_before = fs::read(&manifest_path).unwrap();
        let record_before = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        let projection_before = projection_totals(&fixture);
        let operations_before = fs::read_dir(
            OperationStore::open(fixture.vault.paths.manager())
                .unwrap()
                .operations_root(),
        )
        .unwrap()
        .count();
        let refusal = fixture.service.plan_undeploy(&UndeployPlanRequest {
            deployment_id: deployed.deployment_id.clone(),
            resolution: UndeployResolutionDto::RemoveManaged,
        });
        assert!(matches!(refusal, Err(DeploymentError::DriftBlocked(_))));
        assert!(!target_path.exists());
        assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
        let record_after = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert!(record_after.active);
        assert_eq!(record_after.health, DeploymentHealth::MissingTarget);
        assert_eq!(
            record_after.last_operation_id,
            record_before.last_operation_id
        );
        assert_eq!(record_after.expected_digest, record_before.expected_digest);
        assert_eq!(record_after.updated_at, record_before.updated_at);
        assert_eq!(projection_totals(&fixture), projection_before);
        assert_eq!(
            fs::read_dir(
                OperationStore::open(fixture.vault.paths.manager())
                    .unwrap()
                    .operations_root()
            )
            .unwrap()
            .count(),
            operations_before
        );

        let preserve = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id,
                resolution: UndeployResolutionDto::PreserveTarget,
            })
            .unwrap();
        let operation_id = OperationId::from_str(&preserve.operation_id).unwrap();
        let store = OperationStore::open(fixture.vault.paths.manager()).unwrap();
        let planned = store.load(operation_id).unwrap();
        assert_eq!(
            planned.plan.content.steps[0].action,
            PlanAction::LeaveUntouched
        );
        assert_eq!(
            planned.plan.content.steps[0].before.expected_kind,
            EntryKind::Absent
        );
        assert_eq!(planned.plan.content.steps[0].before.metadata, None);
        execute(&fixture, &preserve);
        assert!(!target_path.exists());
        assert!(!manifest_path.exists());
        let record = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert!(!record.active);
        assert_eq!(record.last_operation_id, Some(operation_id));
        let stored = store.load(operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Finalized);
        assert_eq!(stored.journal.outcome, Some(OperationOutcome::Succeeded));
        assert_projected_success(&fixture, operation_id);
    }

    #[test]
    fn preserve_resolution_does_not_require_mutation_capability() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        fs::write(
            Path::new(&deployed.target_path).join("SKILL.md"),
            "preserve on read only target\n",
        )
        .unwrap();
        let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
            Arc::new(FixedProbe(TargetCapabilityEvidence {
                directory_write: CapabilityStatus::Unsupported,
                atomic_rename: CapabilityStatus::Unsupported,
                symlink: CapabilityStatus::Unsupported,
            })),
            Arc::new(crate::operations::NoopOperationFailpoints),
        );
        let preserve = service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id,
                resolution: UndeployResolutionDto::PreserveTarget,
            })
            .unwrap();
        service
            .execute_operation(&preserve.operation_id, &preserve.plan_digest)
            .unwrap();
        assert_eq!(
            fs::read_to_string(Path::new(&deployed.target_path).join("SKILL.md")).unwrap(),
            "preserve on read only target\n"
        );
    }

    #[test]
    fn create_failpoint_boundaries_leave_no_write_or_durable_committed_evidence() {
        for boundary in [
            OperationBoundary::StageActionApplied(0),
            OperationBoundary::FinalRenamed(0),
            OperationBoundary::VerifyObserved(0),
        ] {
            let fixture = fixture();
            let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
            let reviewed = plan(&fixture, &target, None);
            let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
                Arc::new(FilesystemCapabilityProbe),
                Arc::new(FailAt(boundary)),
            );
            assert!(
                service
                    .execute_operation(&reviewed.operation_id, &reviewed.plan_digest)
                    .is_err()
            );
            assert!(!Path::new(&reviewed.target_path).exists());
            let stored = OperationStore::open(fixture.vault.paths.manager())
                .unwrap()
                .load(OperationId::from_str(&reviewed.operation_id).unwrap())
                .unwrap();
            assert!(matches!(
                stored.journal.outcome,
                Some(OperationOutcome::FailedNoWrites | OperationOutcome::FailedRolledBack)
            ));
        }

        for boundary in [
            OperationBoundary::ManifestsPublished,
            OperationBoundary::ProjectionFinalized,
        ] {
            let fixture = fixture();
            let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
            let reviewed = plan(&fixture, &target, None);
            let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
                Arc::new(FilesystemCapabilityProbe),
                Arc::new(FailAt(boundary)),
            );
            assert!(matches!(
                service.execute_operation(&reviewed.operation_id, &reviewed.plan_digest),
                Err(DeploymentError::Operation(
                    OperationError::FinalizationInterrupted(_)
                ))
            ));
            assert!(Path::new(&reviewed.target_path).is_dir());
            let stored = OperationStore::open(fixture.vault.paths.manager())
                .unwrap()
                .load(OperationId::from_str(&reviewed.operation_id).unwrap())
                .unwrap();
            assert_eq!(stored.journal.state, OperationState::Committed);
            let mut roots = TargetRoots::new();
            roots.insert(
                TargetId::from_str(&reviewed.target_id).unwrap(),
                AuthorizedRoot::open(Path::new(&target.root_path)).unwrap(),
            );
            assert_eq!(
                classify_startup(&stored, &roots).unwrap(),
                StartupDecision::ContinueFinalization
            );
        }
    }

    #[test]
    fn replacement_backup_failure_restores_exact_managed_copy() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let before = hash_bundle(Path::new(&deployed.target_path), BundleCaps::default()).unwrap();
        advance_working(&fixture, "new vault version\n");
        let redeploy = plan(&fixture, &target, None);
        let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
            Arc::new(FilesystemCapabilityProbe),
            Arc::new(FailAt(OperationBoundary::BackupRenamed(0))),
        );
        assert!(
            service
                .execute_operation(&redeploy.operation_id, &redeploy.plan_digest)
                .is_err()
        );
        assert_eq!(
            hash_bundle(Path::new(&deployed.target_path), BundleCaps::default()).unwrap(),
            before
        );
        let stored = OperationStore::open(fixture.vault.paths.manager())
            .unwrap()
            .load(OperationId::from_str(&redeploy.operation_id).unwrap())
            .unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedRolledBack)
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn undeploy_backup_failure_restores_exact_target_and_keeps_relationships_active() {
        let fixture = fixture();
        let primary_target = target(&fixture, "remove-copy", FixtureTargetKindDto::GitProject);
        let other_target = target(&fixture, "other-link", FixtureTargetKindDto::Global);
        let deployed = plan(&fixture, &primary_target, None);
        let other = plan(&fixture, &other_target, None);
        execute(&fixture, &deployed);
        execute(&fixture, &other);
        let target_path = PathBuf::from(&deployed.target_path);
        let other_path = PathBuf::from(&other.target_path);
        let before = hash_bundle(&target_path, BundleCaps::default()).unwrap();
        let before_metadata =
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap());
        let vault_before = hash_bundle(&fixture.working, BundleCaps::default()).unwrap();
        let other_link_before = fs::read_link(&other_path).unwrap();
        let other_metadata_before =
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&other_path).unwrap());
        let deployment_id = DeploymentId::from_str(&deployed.deployment_id).unwrap();
        let other_id = DeploymentId::from_str(&other.deployment_id).unwrap();
        let record_before = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        let other_record_before = fixture
            .vault
            .repositories
            .deployment(other_id)
            .unwrap()
            .unwrap();
        let manifest_path = fixture.vault.manifests.deployment_path(deployment_id);
        let manifest_before = fs::read(&manifest_path).unwrap();
        let totals_before = projection_totals(&fixture);

        let undeploy = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id,
                resolution: UndeployResolutionDto::RemoveManaged,
            })
            .unwrap();
        let operation_id = OperationId::from_str(&undeploy.operation_id).unwrap();
        let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
            Arc::new(FilesystemCapabilityProbe),
            Arc::new(FailAt(OperationBoundary::BackupRenamed(0))),
        );
        assert!(matches!(
            service.execute_operation(&undeploy.operation_id, &undeploy.plan_digest),
            Err(DeploymentError::Operation(
                OperationError::ExecutionFailedRolledBack(_)
            ))
        ));

        assert_eq!(
            hash_bundle(&target_path, BundleCaps::default()).unwrap(),
            before
        );
        assert_eq!(
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&target_path).unwrap()),
            before_metadata
        );
        assert_eq!(
            hash_bundle(&fixture.working, BundleCaps::default()).unwrap(),
            vault_before
        );
        assert_eq!(fs::read_link(&other_path).unwrap(), other_link_before);
        assert_eq!(
            MetadataFingerprint::from_metadata(&fs::symlink_metadata(&other_path).unwrap()),
            other_metadata_before
        );
        assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
        let record_after = fixture
            .vault
            .repositories
            .deployment(deployment_id)
            .unwrap()
            .unwrap();
        assert!(record_after.active);
        assert_eq!(
            record_after.last_operation_id,
            record_before.last_operation_id
        );
        assert_eq!(record_after.expected_digest, record_before.expected_digest);
        assert_eq!(record_after.updated_at, record_before.updated_at);
        let other_record_after = fixture
            .vault
            .repositories
            .deployment(other_id)
            .unwrap()
            .unwrap();
        assert!(other_record_after.active);
        assert_eq!(
            other_record_after.last_operation_id,
            other_record_before.last_operation_id
        );
        assert_eq!(
            projection_totals(&fixture),
            (totals_before.0 + 1, totals_before.1 + 1)
        );
        assert_eq!(operation_activity_counts(&fixture, operation_id), (1, 1));

        let store = OperationStore::open(fixture.vault.paths.manager()).unwrap();
        let stored = store.load(operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Failed);
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedRolledBack)
        );
        let snapshot = read_deployment_snapshot(&store, operation_id).unwrap();
        assert_eq!(snapshot.protections.len(), 1);
        assert_eq!(
            snapshot.protections[0].before.bundle_digest,
            Some(before.digest)
        );
        assert!(snapshot.protections[0].reference.starts_with("object:"));
        assert!(
            fixture
                .vault
                .manifests
                .read_deployment(deployment_id)
                .is_ok()
        );
        assert!(fixture.vault.manifests.read_deployment(other_id).is_ok());
    }

    #[test]
    fn replacement_snapshot_boundary_publishes_recovery_without_active_write() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let before = hash_bundle(Path::new(&deployed.target_path), BundleCaps::default()).unwrap();
        advance_working(&fixture, "new vault version\n");
        let redeploy = plan(&fixture, &target, None);
        let service = DeploymentService::new(Arc::clone(&fixture.vault)).with_test_hooks(
            Arc::new(FilesystemCapabilityProbe),
            Arc::new(FailAt(OperationBoundary::SnapshotPublished)),
        );
        assert!(
            service
                .execute_operation(&redeploy.operation_id, &redeploy.plan_digest)
                .is_err()
        );
        assert_eq!(
            hash_bundle(Path::new(&deployed.target_path), BundleCaps::default()).unwrap(),
            before
        );
        let operation_id = OperationId::from_str(&redeploy.operation_id).unwrap();
        let store = OperationStore::open(fixture.vault.paths.manager()).unwrap();
        let snapshot = read_deployment_snapshot(&store, operation_id).unwrap();
        assert_eq!(
            snapshot.protections[0].before.bundle_digest,
            Some(before.digest)
        );
        let stored = store.load(operation_id).unwrap();
        assert_eq!(
            stored.journal.outcome,
            Some(OperationOutcome::FailedNoWrites)
        );
    }

    #[test]
    #[ignore = "invoked only by child_process_kill_reopens_deployment_evidence"]
    fn deployment_crash_child_helper() {
        let Ok(vault_root) = std::env::var("SKILLS_HUB_DEPLOY_CHILD_VAULT") else {
            return;
        };
        let support = PathBuf::from(
            std::env::var("SKILLS_HUB_DEPLOY_CHILD_SUPPORT").expect("child support path"),
        );
        let marker = PathBuf::from(
            std::env::var("SKILLS_HUB_DEPLOY_CHILD_MARKER").expect("child marker path"),
        );
        let operation_id =
            std::env::var("SKILLS_HUB_DEPLOY_CHILD_OPERATION").expect("child operation ID");
        let plan_digest =
            std::env::var("SKILLS_HUB_DEPLOY_CHILD_DIGEST").expect("child plan digest");
        let boundary =
            if std::env::var("SKILLS_HUB_DEPLOY_CHILD_BOUNDARY").as_deref() == Ok("backup") {
                OperationBoundary::BackupRenamed(0)
            } else {
                OperationBoundary::FinalRenamed(0)
            };
        let vault = Arc::new(OpenVault::open(Path::new(&vault_root), &support, &[]).unwrap());
        let service = DeploymentService::new(vault).with_test_hooks(
            Arc::new(FilesystemCapabilityProbe),
            Arc::new(CrashAt { boundary, marker }),
        );
        let _ = service.execute_operation(&operation_id, &plan_digest);
        panic!("child deployment execution returned before parent killed it");
    }

    #[test]
    fn child_process_kill_reopens_deployment_evidence_and_classifies_without_writes() {
        let fixture = fixture();
        let target = target(&fixture, "copy", FixtureTargetKindDto::GitProject);
        let reviewed = plan(&fixture, &target, None);
        let Fixture {
            temporary,
            vault,
            service,
            skill_id: _,
            working: _,
        } = fixture;
        let vault_root = vault.paths.root().to_path_buf();
        let support = temporary.path().join("support");
        drop(service);
        drop(vault);

        let marker = temporary.path().join("deployment-crash-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "application::deployment::tests::deployment_crash_child_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("SKILLS_HUB_DEPLOY_CHILD_VAULT", &vault_root)
            .env("SKILLS_HUB_DEPLOY_CHILD_SUPPORT", &support)
            .env("SKILLS_HUB_DEPLOY_CHILD_MARKER", &marker)
            .env("SKILLS_HUB_DEPLOY_CHILD_OPERATION", &reviewed.operation_id)
            .env("SKILLS_HUB_DEPLOY_CHILD_DIGEST", &reviewed.plan_digest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not reach the durable deployment rename boundary"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            thread::sleep(Duration::from_millis(20));
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());

        let vault = Arc::new(OpenVault::open(&vault_root, &support, &[]).unwrap());
        let store = OperationStore::open(vault.paths.manager()).unwrap();
        let operation_id = OperationId::from_str(&reviewed.operation_id).unwrap();
        let stored = store.load(operation_id).unwrap();
        assert_eq!(stored.journal.state, OperationState::Committing);
        let context = stored.plan.content.deployment.as_ref().unwrap();
        let mut roots = TargetRoots::new();
        roots.insert(
            context.target.target_id,
            AuthorizedRoot::open(Path::new(&context.target.target_root)).unwrap(),
        );
        let before = (
            hash_bundle(Path::new(&reviewed.target_path), BundleCaps::default()).unwrap(),
            fs::read(store.operation_directory(operation_id).join("journal.json")).unwrap(),
        );
        assert_eq!(
            classify_startup(&stored, &roots).unwrap(),
            StartupDecision::ContinueVerification
        );
        assert_eq!(
            classify_startup(&stored, &roots).unwrap(),
            StartupDecision::ContinueVerification
        );
        let after = (
            hash_bundle(Path::new(&reviewed.target_path), BundleCaps::default()).unwrap(),
            fs::read(store.operation_directory(operation_id).join("journal.json")).unwrap(),
        );
        assert_eq!(after, before, "startup classification must be read-only");

        let recovery = DeploymentService::new(Arc::clone(&vault));
        let recovered = recovery.recover_operation(operation_id).unwrap();
        assert_eq!(recovered.outcome, OperationOutcome::Succeeded);
        let terminal_tree =
            hash_bundle(Path::new(&reviewed.target_path), BundleCaps::default()).unwrap();
        let terminal_journal = fs::read(store.journal_path(operation_id)).unwrap();
        assert!(
            vault
                .repositories
                .deployment(DeploymentId::from_str(&reviewed.deployment_id).unwrap())
                .unwrap()
                .is_some_and(|deployment| deployment.active)
        );
        assert!(
            vault
                .manifests
                .read_deployment(DeploymentId::from_str(&reviewed.deployment_id).unwrap())
                .is_ok()
        );
        let replay = recovery.recover_operation(operation_id).unwrap();
        assert!(replay.replayed);
        assert_eq!(
            hash_bundle(Path::new(&reviewed.target_path), BundleCaps::default()).unwrap(),
            terminal_tree
        );
        assert_eq!(
            fs::read(store.journal_path(operation_id)).unwrap(),
            terminal_journal
        );
    }

    #[test]
    fn child_process_kill_reopens_and_finishes_undeploy_idempotently() {
        let fixture = fixture();
        let target = target(&fixture, "remove-copy", FixtureTargetKindDto::GitProject);
        let deployed = plan(&fixture, &target, None);
        execute(&fixture, &deployed);
        let reviewed = fixture
            .service
            .plan_undeploy(&UndeployPlanRequest {
                deployment_id: deployed.deployment_id.clone(),
                resolution: UndeployResolutionDto::RemoveManaged,
            })
            .unwrap();
        let target_path = PathBuf::from(&deployed.target_path);
        let deployment_id = DeploymentId::from_str(&deployed.deployment_id).unwrap();
        let Fixture {
            temporary,
            vault,
            service,
            skill_id: _,
            working: _,
        } = fixture;
        let vault_root = vault.paths.root().to_path_buf();
        let support = temporary.path().join("support");
        drop(service);
        drop(vault);

        let marker = temporary.path().join("undeploy-crash-ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "application::deployment::tests::deployment_crash_child_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("SKILLS_HUB_DEPLOY_CHILD_VAULT", &vault_root)
            .env("SKILLS_HUB_DEPLOY_CHILD_SUPPORT", &support)
            .env("SKILLS_HUB_DEPLOY_CHILD_MARKER", &marker)
            .env("SKILLS_HUB_DEPLOY_CHILD_OPERATION", &reviewed.operation_id)
            .env("SKILLS_HUB_DEPLOY_CHILD_DIGEST", &reviewed.plan_digest)
            .env("SKILLS_HUB_DEPLOY_CHILD_BOUNDARY", "backup")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not reach the durable undeploy rename boundary"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            thread::sleep(Duration::from_millis(20));
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());

        let vault = Arc::new(OpenVault::open(&vault_root, &support, &[]).unwrap());
        let recovery = DeploymentService::new(Arc::clone(&vault));
        let operation_id = OperationId::from_str(&reviewed.operation_id).unwrap();
        assert!(matches!(
            recovery.recover_operation(operation_id),
            Err(DeploymentError::Operation(
                OperationError::ExecutionFailedRolledBack(_)
            ))
        ));
        assert!(target_path.is_dir());
        assert!(
            vault
                .repositories
                .deployment(deployment_id)
                .unwrap()
                .is_some_and(|deployment| deployment.active)
        );
        let second = recovery.recover_operation(operation_id).unwrap();
        assert_eq!(second.outcome, OperationOutcome::FailedRolledBack);
        assert!(second.replayed);
        assert!(target_path.is_dir());
    }
}
