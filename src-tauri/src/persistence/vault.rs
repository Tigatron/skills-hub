use std::{
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use thiserror::Error;

use crate::{
    domain::{DeploymentName, SkillId, TrashEntryId, UtcTimestamp},
    filesystem::ObjectStore,
};

use super::{
    executor::{DbExecutor, DbExecutorError},
    manifests::{
        DeviceSettings, ManifestError, ManifestStore, VaultManifest, read_versioned,
        write_versioned,
    },
    repositories::Repositories,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPaths {
    root: PathBuf,
    manager: PathBuf,
}

impl VaultPaths {
    fn new(root: PathBuf) -> Self {
        let manager = root.join(".manager");
        Self { root, manager }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn manager(&self) -> &Path {
        &self.manager
    }

    #[must_use]
    pub fn skills(&self) -> PathBuf {
        self.root.join("skills")
    }

    #[must_use]
    pub fn vault_manifest(&self) -> PathBuf {
        self.manager.join("vault.json")
    }

    #[must_use]
    pub fn database(&self) -> PathBuf {
        self.manager.join("index.sqlite")
    }

    #[must_use]
    pub fn objects(&self) -> PathBuf {
        self.manager.join("objects")
    }

    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.manager.join("staging")
    }

    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.manager.join("locks/vault.lock")
    }

    #[must_use]
    pub fn working_bundle(&self, id: SkillId, deployment_name: &DeploymentName) -> PathBuf {
        self.skills()
            .join(id.to_string())
            .join(deployment_name.as_str())
    }

    #[must_use]
    pub fn skill_container(&self, id: SkillId) -> PathBuf {
        self.skills().join(id.to_string())
    }

    #[must_use]
    pub fn trash(&self) -> PathBuf {
        self.manager.join("trash")
    }

    #[must_use]
    pub fn trash_skill(&self, id: SkillId) -> PathBuf {
        self.trash().join(id.to_string())
    }

    #[must_use]
    pub fn trash_entry(&self, _skill_id: SkillId, entry_id: TrashEntryId) -> PathBuf {
        self.trash().join(entry_id.to_string())
    }

    #[must_use]
    pub fn trash_entry_bundle(&self, skill_id: SkillId, entry_id: TrashEntryId) -> PathBuf {
        self.trash_entry(skill_id, entry_id).join("working")
    }

    #[must_use]
    pub fn trash_entry_manifest(&self, skill_id: SkillId, entry_id: TrashEntryId) -> PathBuf {
        let _ = skill_id;
        self.trash_entry(skill_id, entry_id).join("manifest.json")
    }

    fn required_directories(&self) -> Vec<PathBuf> {
        [
            self.skills(),
            self.manager.clone(),
            self.manager.join("manifests"),
            self.manager.join("manifests/skills"),
            self.manager.join("manifests/deployments"),
            self.objects(),
            self.manager.join("objects/sha256-bundle-v1"),
            self.staging(),
            self.manager.join("trash"),
            self.manager.join("operations"),
            self.manager.join("locks"),
        ]
        .into_iter()
        .collect()
    }
}

pub struct OpenVault {
    pub paths: VaultPaths,
    pub manifest: VaultManifest,
    pub database: DbExecutor,
    pub repositories: Repositories,
    pub manifests: ManifestStore,
    pub objects: ObjectStore,
    _lock: VaultLock,
}

impl OpenVault {
    /// Initializes or reopens a Vault while holding its process-lifetime advisory lock.
    ///
    /// `application_support` is the device-local `Skills Hub` directory containing
    /// `settings.json`; it must stay outside the selected Vault.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe nesting, aliases, lock conflicts, invalid manifests,
    /// layout conflicts, or database migration failures.
    pub fn open(
        selected_root: &Path,
        application_support: &Path,
        configured_targets: &[PathBuf],
    ) -> Result<Self, VaultError> {
        if !selected_root.is_absolute() || !application_support.is_absolute() {
            return Err(VaultError::PathNotAbsolute);
        }
        ensure_directory(selected_root)?;
        let canonical_root = selected_root.canonicalize()?;
        let paths = VaultPaths::new(canonical_root);
        validate_nesting(&paths, application_support, configured_targets)?;

        ensure_directory(&paths.manager)?;
        ensure_directory(&paths.manager.join("locks"))?;
        let vault_lock = VaultLock::acquire(&paths.lock_file())?;
        for directory in paths.required_directories() {
            ensure_directory(&directory)?;
        }

        let vault_manifest = if paths.vault_manifest().exists() {
            read_versioned(&paths.vault_manifest())?
        } else {
            let manifest = VaultManifest::new(UtcTimestamp::now());
            write_versioned(&paths.vault_manifest(), &manifest)?;
            manifest
        };

        ensure_directory(application_support)?;
        let _settings_lock = lock_device_settings(application_support)?;
        let settings_path = application_support.join("settings.json");
        let settings = if settings_path.exists() {
            match read_versioned::<DeviceSettings>(&settings_path) {
                Ok(mut settings) => {
                    settings.active_vault_path.clone_from(&paths.root);
                    settings
                }
                Err(error @ ManifestError::UnsupportedSchema { .. }) => return Err(error.into()),
                Err(_) => DeviceSettings::new(paths.root.clone()),
            }
        } else {
            DeviceSettings::new(paths.root.clone())
        };
        write_versioned(&settings_path, &settings)?;

        let database = DbExecutor::open(paths.database())?;
        let repositories = Repositories::new(database.clone());
        let manifests = ManifestStore::new(&paths.manager);
        let objects = ObjectStore::new(paths.objects(), paths.staging());
        Ok(Self {
            paths,
            manifest: vault_manifest,
            database,
            repositories,
            manifests,
            objects,
            _lock: vault_lock,
        })
    }
}

#[must_use]
pub fn default_application_support(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Skills Hub")
}

#[must_use]
pub fn default_vault_path(home: &Path) -> PathBuf {
    default_application_support(home).join("Vault")
}

/// Reads existing device settings without creating a Vault or settings directory.
///
/// # Errors
///
/// Returns an error when an existing settings file is malformed or unsupported.
pub fn existing_device_settings(
    application_support: &Path,
) -> Result<Option<DeviceSettings>, VaultError> {
    let path = application_support.join("settings.json");
    if !path.exists() {
        return Ok(None);
    }
    read_versioned(&path)
        .map(Some)
        .map_err(VaultError::Manifest)
}

/// Atomically updates the device-local debug logging preference without requiring a Vault.
///
/// # Errors
///
/// Returns an error when settings are absent, invalid, or cannot be durably replaced.
pub(crate) fn update_debug_logging(
    application_support: &Path,
    enabled: bool,
) -> Result<(), VaultError> {
    mutate_device_settings(application_support, |settings| {
        settings.debug_logging = enabled;
        true
    })
    .map(|_| ())
    .map_err(VaultError::Manifest)
}

/// Atomically changes only the active Vault path when its current value still matches review.
/// Other settings fields are reread under the device-settings lock and preserved.
///
/// # Errors
///
/// Returns an error when settings cannot be locked, read, validated, or durably replaced.
pub(crate) fn update_active_vault_path(
    application_support: &Path,
    expected: &Path,
    replacement: &Path,
) -> Result<bool, ManifestError> {
    mutate_device_settings(application_support, |settings| {
        if settings.active_vault_path != expected {
            return false;
        }
        settings.active_vault_path = replacement.to_path_buf();
        true
    })
}

fn mutate_device_settings<F>(application_support: &Path, mutate: F) -> Result<bool, ManifestError>
where
    F: FnOnce(&mut DeviceSettings) -> bool,
{
    let _lock = lock_device_settings(application_support)?;
    let path = application_support.join("settings.json");
    let mut settings: DeviceSettings = read_versioned(&path)?;
    if !mutate(&mut settings) {
        return Ok(false);
    }
    write_versioned(&path, &settings)?;
    Ok(true)
}

fn lock_device_settings(application_support: &Path) -> Result<File, ManifestError> {
    let lock_path = application_support.join("settings.mutation.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(ManifestError::Io)?;
    lock.lock_exclusive().map_err(ManifestError::Io)?;
    Ok(lock)
}

fn validate_nesting(
    paths: &VaultPaths,
    application_support: &Path,
    configured_targets: &[PathBuf],
) -> Result<(), VaultError> {
    let settings_parent = canonicalize_existing_or_parent(application_support)?;
    if settings_parent.starts_with(paths.root()) {
        return Err(VaultError::SettingsInsideVault);
    }

    for target in configured_targets {
        if !target.is_absolute() {
            return Err(VaultError::PathNotAbsolute);
        }
        let canonical_target = canonicalize_existing_or_parent(target)?;
        if canonical_target == paths.root() {
            return Err(VaultError::PathAlias {
                target: target.clone(),
            });
        }
        if paths.root().starts_with(&canonical_target) {
            return Err(VaultError::VaultInsideTarget {
                target: target.clone(),
            });
        }
        if canonical_target.starts_with(paths.manager()) {
            return Err(VaultError::TargetInsideManager {
                target: target.clone(),
            });
        }
    }
    Ok(())
}

fn canonicalize_existing_or_parent(path: &Path) -> Result<PathBuf, VaultError> {
    if path.exists() {
        return path.canonicalize().map_err(VaultError::Io);
    }
    let mut missing = Vec::new();
    let mut ancestor = path;
    while !ancestor.exists() {
        let component = ancestor.file_name().ok_or(VaultError::NoExistingAncestor)?;
        missing.push(component.to_owned());
        ancestor = ancestor.parent().ok_or(VaultError::NoExistingAncestor)?;
    }
    let mut canonical = ancestor.canonicalize()?;
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn ensure_directory(path: &Path) -> Result<(), VaultError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(VaultError::LayoutConflict {
                path: path.to_path_buf(),
            })
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            Ok(())
        }
        Err(error) => Err(VaultError::Io(error)),
    }
}

struct VaultLock {
    file: File,
}

impl VaultLock {
    fn acquire(path: &Path) -> Result<Self, VaultError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock_exclusive().map_err(|source| {
            if source.kind() == io::ErrorKind::WouldBlock {
                VaultError::AlreadyOpen
            } else {
                VaultError::Lock(source)
            }
        })?;

        let metadata = serde_json::json!({
            "pid": std::process::id(),
            "acquiredAt": UtcTimestamp::now(),
            "diagnosticOnly": true
        });
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        serde_json::to_writer_pretty(&mut file, &metadata)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(Self { file })
    }
}

impl Drop for VaultLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("Vault, settings, and target paths must be absolute")]
    PathNotAbsolute,
    #[error("Vault filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("Vault layout path is a symlink or non-directory: {path:?}")]
    LayoutConflict { path: PathBuf },
    #[error("Vault path resolution found no existing ancestor")]
    NoExistingAncestor,
    #[error("device settings directory cannot be inside the Vault")]
    SettingsInsideVault,
    #[error("Vault cannot be nested inside configured target {target:?}")]
    VaultInsideTarget { target: PathBuf },
    #[error("configured target cannot be nested inside Vault internals: {target:?}")]
    TargetInsideManager { target: PathBuf },
    #[error("configured target aliases the Vault: {target:?}")]
    PathAlias { target: PathBuf },
    #[error("this Vault is already open by another process")]
    AlreadyOpen,
    #[error("Vault advisory lock failed: {0}")]
    Lock(io::Error),
    #[error("Vault manifest/settings failed: {0}")]
    Manifest(#[from] ManifestError),
    #[error("Vault database failed: {0}")]
    Database(#[from] DbExecutorError),
    #[error("Vault lock metadata JSON failed: {0}")]
    LockMetadata(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{BundleDigest, DeploymentName, SkillId},
        persistence::SkillManifest,
    };
    use std::sync::{Arc, Barrier};

    #[test]
    fn default_path_matches_the_accepted_macos_location() {
        assert_eq!(
            default_vault_path(Path::new("/Users/test")),
            Path::new("/Users/test/Library/Application Support/Skills Hub/Vault")
        );
    }

    #[test]
    fn debug_logging_preference_is_durable_without_mutating_vault_content() {
        let home = tempfile::tempdir().unwrap();
        let application_support = default_application_support(home.path());
        let vault = default_vault_path(home.path());
        drop(OpenVault::open(&vault, &application_support, &[]).unwrap());

        update_debug_logging(&application_support, true).unwrap();
        let relocated = home.path().join("relocated-vault");
        assert!(
            update_active_vault_path(
                &application_support,
                &vault.canonicalize().unwrap(),
                &relocated
            )
            .unwrap()
        );

        let settings = existing_device_settings(&application_support)
            .unwrap()
            .unwrap();
        assert!(settings.debug_logging);
        assert_eq!(settings.active_vault_path, relocated);
        assert!(vault.join(".manager/vault.json").is_file());

        update_debug_logging(&application_support, false).unwrap();
        let settings = existing_device_settings(&application_support)
            .unwrap()
            .unwrap();
        assert!(!settings.debug_logging);
        assert_eq!(settings.active_vault_path, relocated);
    }

    #[test]
    fn concurrent_vault_open_and_debug_update_preserve_both_settings() {
        let home = tempfile::tempdir().unwrap();
        let application_support = default_application_support(home.path());
        let initial = home.path().join("initial-vault");
        drop(OpenVault::open(&initial, &application_support, &[]).unwrap());
        let selected = home.path().join("selected-vault");
        let barrier = Arc::new(Barrier::new(3));

        let open_barrier = Arc::clone(&barrier);
        let open_support = application_support.clone();
        let open_selected = selected.clone();
        let open = std::thread::spawn(move || {
            open_barrier.wait();
            drop(OpenVault::open(&open_selected, &open_support, &[]).unwrap());
        });
        let debug_barrier = Arc::clone(&barrier);
        let debug_support = application_support.clone();
        let debug = std::thread::spawn(move || {
            debug_barrier.wait();
            update_debug_logging(&debug_support, true).unwrap();
        });
        barrier.wait();
        open.join().unwrap();
        debug.join().unwrap();

        let settings = existing_device_settings(&application_support)
            .unwrap()
            .unwrap();
        assert!(settings.debug_logging);
        assert_eq!(settings.active_vault_path, selected.canonicalize().unwrap());
    }

    #[test]
    fn clean_vault_initializes_and_reopens_with_stable_identity() {
        let directory = tempfile::tempdir().unwrap();
        let vault_path = directory.path().join("vault");
        let settings = directory.path().join("application-support");
        let first = OpenVault::open(&vault_path, &settings, &[]).unwrap();
        let vault_id = first.manifest.vault_id;
        for expected in first.paths.required_directories() {
            assert!(expected.is_dir(), "{}", expected.display());
        }
        assert_eq!(first.database.settings().unwrap().schema_version, 5);
        drop(first);

        let second = OpenVault::open(&vault_path, &settings, &[]).unwrap();
        assert_eq!(second.manifest.vault_id, vault_id);
        assert_eq!(
            read_versioned::<DeviceSettings>(&settings.join("settings.json"))
                .unwrap()
                .active_vault_path,
            vault_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn advisory_lock_conflict_blocks_a_second_open_even_if_metadata_exists() {
        let directory = tempfile::tempdir().unwrap();
        let vault_path = directory.path().join("vault");
        let settings = directory.path().join("application-support");
        let first = OpenVault::open(&vault_path, &settings, &[]).unwrap();

        assert!(matches!(
            OpenVault::open(&vault_path, &settings, &[]),
            Err(VaultError::AlreadyOpen)
        ));
        drop(first);
        assert!(OpenVault::open(&vault_path, &settings, &[]).is_ok());
    }

    #[test]
    fn corrupt_device_settings_are_preserved_and_regenerated() {
        let directory = tempfile::tempdir().unwrap();
        let vault_path = directory.path().join("vault");
        let settings = directory.path().join("application-support");
        fs::create_dir(&settings).unwrap();
        fs::write(settings.join("settings.json"), b"{truncated").unwrap();

        let vault = OpenVault::open(&vault_path, &settings, &[]).unwrap();

        assert_eq!(
            read_versioned::<DeviceSettings>(&settings.join("settings.json"))
                .unwrap()
                .active_vault_path,
            vault.paths.root()
        );
        assert!(fs::read_dir(&settings).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("settings.json.corrupt-")
        }));
    }

    #[test]
    fn vault_and_manager_target_nesting_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        let nested_vault = target.join("vault");
        let settings = directory.path().join("application-support");
        assert!(matches!(
            OpenVault::open(&nested_vault, &settings, std::slice::from_ref(&target)),
            Err(VaultError::VaultInsideTarget { .. })
        ));

        let vault = directory.path().join("separate-vault");
        let internal_target = vault.join(".manager/custom-target");
        assert!(matches!(
            OpenVault::open(&vault, &settings, &[internal_target]),
            Err(VaultError::TargetInsideManager { .. })
        ));
    }

    #[test]
    fn uuid_containers_allow_same_deployment_name_without_collision() {
        let directory = tempfile::tempdir().unwrap();
        let vault = OpenVault::open(
            &directory.path().join("vault"),
            &directory.path().join("application-support"),
            &[],
        )
        .unwrap();
        let name = DeploymentName::parse("same-name").unwrap();
        let first = vault.paths.working_bundle(SkillId::generate(), &name);
        let second = vault.paths.working_bundle(SkillId::generate(), &name);

        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        assert_ne!(first.parent(), second.parent());
        assert_eq!(first.file_name(), second.file_name());
    }

    #[test]
    fn readable_working_content_and_manifest_survive_index_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let vault_path = directory.path().join("vault");
        let settings = directory.path().join("application-support");
        let vault = OpenVault::open(&vault_path, &settings, &[]).unwrap();
        let skill_id = SkillId::generate();
        let name = DeploymentName::parse("readable").unwrap();
        let working = vault.paths.working_bundle(skill_id, &name);
        fs::create_dir_all(&working).unwrap();
        fs::write(working.join("SKILL.md"), "readable bytes\n").unwrap();
        let manifest = SkillManifest::new(
            skill_id,
            "Readable".to_owned(),
            name,
            BundleDigest::from_bytes([1; 32]),
            BundleDigest::from_bytes([1; 32]),
            UtcTimestamp::from_unix_millis(1_000).unwrap(),
            Vec::new(),
        )
        .unwrap();
        vault.manifests.write_skill(&manifest).unwrap();
        let database = vault.paths.database();
        let manifest_path = vault.manifests.skill_path(skill_id);
        drop(vault);

        fs::remove_file(database).unwrap();
        assert_eq!(
            fs::read_to_string(working.join("SKILL.md")).unwrap(),
            "readable bytes\n"
        );
        assert_eq!(
            read_versioned::<SkillManifest>(&manifest_path).unwrap(),
            manifest
        );
    }
}
