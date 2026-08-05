use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{
    domain::{
        AdapterId, BundleDigest, BundleRelativePath, DeploymentId, DeploymentMode, DeploymentName,
        OperationId, SkillId, TargetId, UtcTimestamp, VaultId,
    },
    filesystem::durable::{atomic_write, preserve_corrupt_copy, sync_directory},
};

pub const VAULT_SCHEMA_VERSION: u32 = 1;
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
pub const SKILL_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const DEPLOYMENT_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const DIGEST_VERSION: &str = "sha256-bundle-v1";

pub(crate) trait VersionedManifest: Serialize + DeserializeOwned {
    const SCHEMA_VERSION: u32;

    fn validate(&self) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VaultManifest {
    pub schema_version: u32,
    pub vault_id: VaultId,
    pub digest_version: String,
    pub trash_policy: TrashPolicy,
    pub created_at: UtcTimestamp,
    pub minimum_compatible_app_version: String,
    pub maximum_compatible_app_version: String,
}

impl VaultManifest {
    #[must_use]
    pub fn new(created_at: UtcTimestamp) -> Self {
        Self {
            schema_version: VAULT_SCHEMA_VERSION,
            vault_id: VaultId::generate(),
            digest_version: DIGEST_VERSION.to_owned(),
            trash_policy: TrashPolicy::default(),
            created_at,
            minimum_compatible_app_version: env!("CARGO_PKG_VERSION").to_owned(),
            maximum_compatible_app_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

impl VersionedManifest for VaultManifest {
    const SCHEMA_VERSION: u32 = VAULT_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err("Vault schemaVersion does not match the supported version".to_owned());
        }
        if self.digest_version != DIGEST_VERSION {
            return Err("Vault digestVersion is unsupported".to_owned());
        }
        if self.minimum_compatible_app_version.is_empty()
            || self.maximum_compatible_app_version.is_empty()
        {
            return Err("Vault application compatibility range is empty".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrashPolicy {
    RetainUntilExplicitDelete,
}

impl Default for TrashPolicy {
    fn default() -> Self {
        Self::RetainUntilExplicitDelete
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceSettings {
    pub schema_version: u32,
    pub active_vault_path: PathBuf,
    pub workspace_roots: Vec<PathBuf>,
    pub target_overrides: BTreeMap<AdapterId, PathBuf>,
    pub custom_target_paths: Vec<PathBuf>,
    pub appearance: Appearance,
    pub debug_logging: bool,
}

impl DeviceSettings {
    #[must_use]
    pub fn new(active_vault_path: PathBuf) -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_vault_path,
            workspace_roots: Vec::new(),
            target_overrides: BTreeMap::new(),
            custom_target_paths: Vec::new(),
            appearance: Appearance::System,
            debug_logging: false,
        }
    }
}

impl VersionedManifest for DeviceSettings {
    const SCHEMA_VERSION: u32 = SETTINGS_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err("settings schemaVersion does not match the supported version".to_owned());
        }
        let paths = std::iter::once(&self.active_vault_path)
            .chain(self.workspace_roots.iter())
            .chain(self.target_overrides.values())
            .chain(self.custom_target_paths.iter());
        if paths.into_iter().any(|path| !path.is_absolute()) {
            return Err("settings paths must be absolute".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillManifest {
    pub schema_version: u32,
    pub skill_id: SkillId,
    pub display_name: String,
    pub deployment_name: DeploymentName,
    pub working_path: BundleRelativePath,
    pub working_digest: BundleDigest,
    pub baseline_digest: BundleDigest,
    pub created_at: UtcTimestamp,
    pub sources: Vec<SkillManifestSource>,
}

impl SkillManifest {
    /// Builds a manifest with the only valid working path for this Skill identity and name.
    ///
    /// # Errors
    ///
    /// Returns an error only if the generated path violates the frozen path policy.
    pub fn new(
        skill_id: SkillId,
        display_name: String,
        deployment_name: DeploymentName,
        working_digest: BundleDigest,
        baseline_digest: BundleDigest,
        created_at: UtcTimestamp,
        sources: Vec<SkillManifestSource>,
    ) -> Result<Self, ManifestError> {
        let working_path =
            BundleRelativePath::parse(&format!("skills/{skill_id}/{}", deployment_name.as_str()))
                .map_err(|source| ManifestError::InvalidValue {
                reason: source.to_string(),
            })?;
        Ok(Self {
            schema_version: SKILL_MANIFEST_SCHEMA_VERSION,
            skill_id,
            display_name,
            deployment_name,
            working_path,
            working_digest,
            baseline_digest,
            created_at,
            sources,
        })
    }
}

impl VersionedManifest for SkillManifest {
    const SCHEMA_VERSION: u32 = SKILL_MANIFEST_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(
                "Skill manifest schemaVersion does not match the supported version".to_owned(),
            );
        }
        if self.display_name.trim().is_empty() || self.display_name.chars().any(char::is_control) {
            return Err("Skill displayName is empty or contains control characters".to_owned());
        }
        let expected = format!("skills/{}/{}", self.skill_id, self.deployment_name.as_str());
        if self.working_path.as_str() != expected {
            return Err("Skill workingPath does not match its stable identity and name".to_owned());
        }
        for source in &self.sources {
            source.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillManifestSource {
    pub kind: LocalSourceKind,
    pub path: PathBuf,
    pub captured_at: UtcTimestamp,
    pub confidence: SourceConfidence,
}

impl SkillManifestSource {
    fn validate(&self) -> Result<(), String> {
        if !self.path.is_absolute() {
            return Err("Skill source path must be absolute".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalSourceKind {
    LocalObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceConfidence {
    Observed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentManifest {
    pub schema_version: u32,
    pub deployment_id: DeploymentId,
    pub skill_id: SkillId,
    pub target_id: TargetId,
    pub deployment_name: DeploymentName,
    pub mode: DeploymentMode,
    pub target_path: PathBuf,
    pub expected_digest: BundleDigest,
    pub expected_link_target: Option<PathBuf>,
    pub adapter_version: AdapterId,
    pub last_finalized_operation_id: OperationId,
    pub verified_at: UtcTimestamp,
}

impl VersionedManifest for DeploymentManifest {
    const SCHEMA_VERSION: u32 = DEPLOYMENT_MANIFEST_SCHEMA_VERSION;

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(
                "deployment manifest schemaVersion does not match the supported version".to_owned(),
            );
        }
        if !self.target_path.is_absolute() {
            return Err("deployment targetPath must be absolute".to_owned());
        }
        match (self.mode, self.expected_link_target.as_ref()) {
            (DeploymentMode::Symlink, Some(path)) if path.is_absolute() => Ok(()),
            (DeploymentMode::ManagedCopy, None) => Ok(()),
            (DeploymentMode::Symlink, _) => {
                Err("symlink deployment requires an absolute expectedLinkTarget".to_owned())
            }
            (DeploymentMode::ManagedCopy, Some(_)) => {
                Err("Managed Copy deployment cannot contain expectedLinkTarget".to_owned())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManifestStore {
    skills: PathBuf,
    deployments: PathBuf,
}

impl ManifestStore {
    #[must_use]
    pub fn new(manager_root: &Path) -> Self {
        Self {
            skills: manager_root.join("manifests/skills"),
            deployments: manager_root.join("manifests/deployments"),
        }
    }

    #[must_use]
    pub fn skill_path(&self, id: SkillId) -> PathBuf {
        self.skills.join(format!("{id}.json"))
    }

    #[must_use]
    pub fn deployment_path(&self, id: DeploymentId) -> PathBuf {
        self.deployments.join(format!("{id}.json"))
    }

    /// Atomically writes a validated, readable Skill manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, preservation, serialization, or durable writing fails.
    pub fn write_skill(&self, manifest: &SkillManifest) -> Result<(), ManifestError> {
        write_versioned(&self.skill_path(manifest.skill_id), manifest)
    }

    /// Reads and validates one Skill manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is unavailable, malformed, unsupported, or inconsistent.
    pub fn read_skill(&self, id: SkillId) -> Result<SkillManifest, ManifestError> {
        read_versioned(&self.skill_path(id))
    }

    /// Atomically writes a validated, readable deployment manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when validation, preservation, serialization, or durable writing fails.
    pub fn write_deployment(&self, manifest: &DeploymentManifest) -> Result<(), ManifestError> {
        write_versioned(&self.deployment_path(manifest.deployment_id), manifest)
    }

    /// Reads and validates one deployment manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is unavailable, malformed, unsupported, or inconsistent.
    pub fn read_deployment(&self, id: DeploymentId) -> Result<DeploymentManifest, ManifestError> {
        read_versioned(&self.deployment_path(id))
    }

    /// Durably removes one exact deployment manifest. Repeating removal is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact manifest cannot be removed or its parent cannot be synced.
    pub fn remove_deployment(&self, id: DeploymentId) -> Result<(), ManifestError> {
        let path = self.deployment_path(id);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.deployments)
                .map_err(|error| ManifestError::Durability(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ManifestError::Io(error)),
        }
    }
}

pub(crate) fn read_versioned<T: VersionedManifest>(path: &Path) -> Result<T, ManifestError> {
    let bytes = fs::read(path).map_err(ManifestError::Io)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(ManifestError::InvalidJson)?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or(ManifestError::MissingSchemaVersion)?;
    if schema_version != u64::from(T::SCHEMA_VERSION) {
        return Err(ManifestError::UnsupportedSchema {
            expected: T::SCHEMA_VERSION,
            found: schema_version,
        });
    }
    let manifest: T = serde_json::from_value(value).map_err(ManifestError::InvalidJson)?;
    manifest
        .validate()
        .map_err(|reason| ManifestError::InvalidValue { reason })?;
    Ok(manifest)
}

pub(crate) fn write_versioned<T: VersionedManifest>(
    path: &Path,
    value: &T,
) -> Result<(), ManifestError> {
    value
        .validate()
        .map_err(|reason| ManifestError::InvalidValue { reason })?;
    if path.exists() {
        match read_versioned::<T>(path) {
            Ok(_) => {}
            Err(ManifestError::UnsupportedSchema { expected, found }) => {
                return Err(ManifestError::UnsupportedSchema { expected, found });
            }
            Err(_) => {
                preserve_corrupt_copy(path)
                    .map_err(|source| ManifestError::Durability(source.to_string()))?;
            }
        }
    }

    let mut bytes = serde_json::to_vec_pretty(value).map_err(ManifestError::InvalidJson)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes).map_err(|source| ManifestError::Durability(source.to_string()))
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest filesystem operation failed: {0}")]
    Io(std::io::Error),
    #[error("manifest is not valid JSON: {0}")]
    InvalidJson(serde_json::Error),
    #[error("manifest does not contain an integer schemaVersion")]
    MissingSchemaVersion,
    #[error("manifest schemaVersion {found} is unsupported; expected {expected}")]
    UnsupportedSchema { expected: u32, found: u64 },
    #[error("manifest value is invalid: {reason}")]
    InvalidValue { reason: String },
    #[error("durable manifest operation failed: {0}")]
    Durability(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> BundleDigest {
        BundleDigest::from_bytes([byte; 32])
    }

    #[test]
    fn skill_manifest_is_readable_and_rejects_identity_path_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let manager = directory.path().join(".manager");
        fs::create_dir_all(manager.join("manifests/skills")).unwrap();
        fs::create_dir_all(manager.join("manifests/deployments")).unwrap();
        let store = ManifestStore::new(&manager);
        let skill_id = SkillId::generate();
        let manifest = SkillManifest::new(
            skill_id,
            "Frontend Design".to_owned(),
            DeploymentName::parse("frontend-design").unwrap(),
            digest(1),
            digest(2),
            UtcTimestamp::from_unix_millis(1_000).unwrap(),
            Vec::new(),
        )
        .unwrap();

        store.write_skill(&manifest).unwrap();

        assert_eq!(store.read_skill(skill_id).unwrap(), manifest);
        let text = fs::read_to_string(store.skill_path(skill_id)).unwrap();
        assert!(text.contains("\"schemaVersion\": 1"));
        assert!(text.ends_with('\n'));

        let mut invalid = manifest;
        invalid.working_path = BundleRelativePath::parse("skills/other/name").unwrap();
        assert!(matches!(
            store.write_skill(&invalid),
            Err(ManifestError::InvalidValue { .. })
        ));
    }

    #[test]
    fn unsupported_future_schema_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let manager = directory.path().join(".manager");
        fs::create_dir_all(manager.join("manifests/skills")).unwrap();
        fs::create_dir_all(manager.join("manifests/deployments")).unwrap();
        let store = ManifestStore::new(&manager);
        let skill_id = SkillId::generate();
        let path = store.skill_path(skill_id);
        fs::write(&path, b"{\"schemaVersion\":99}").unwrap();
        let manifest = SkillManifest::new(
            skill_id,
            "Name".to_owned(),
            DeploymentName::parse("name").unwrap(),
            digest(1),
            digest(1),
            UtcTimestamp::from_unix_millis(1_000).unwrap(),
            Vec::new(),
        )
        .unwrap();

        assert!(matches!(
            store.write_skill(&manifest),
            Err(ManifestError::UnsupportedSchema { found: 99, .. })
        ));
        assert_eq!(fs::read(&path).unwrap(), b"{\"schemaVersion\":99}");
    }

    #[test]
    fn replacing_malformed_json_preserves_a_diagnostic_copy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        fs::write(&path, b"{malformed").unwrap();
        let settings = DeviceSettings::new(directory.path().join("vault"));

        write_versioned(&path, &settings).unwrap();

        assert_eq!(read_versioned::<DeviceSettings>(&path).unwrap(), settings);
        assert!(fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("settings.json.corrupt-")
        }));
    }

    #[test]
    fn deployment_mode_and_link_metadata_must_agree() {
        let manifest = DeploymentManifest {
            schema_version: DEPLOYMENT_MANIFEST_SCHEMA_VERSION,
            deployment_id: DeploymentId::generate(),
            skill_id: SkillId::generate(),
            target_id: TargetId::generate(),
            deployment_name: DeploymentName::parse("skill").unwrap(),
            mode: DeploymentMode::Symlink,
            target_path: PathBuf::from("/tmp/target"),
            expected_digest: digest(1),
            expected_link_target: None,
            adapter_version: "claude-code@1".parse().unwrap(),
            last_finalized_operation_id: OperationId::generate(),
            verified_at: UtcTimestamp::from_unix_millis(1_000).unwrap(),
        };

        assert!(manifest.validate().is_err());
    }
}
