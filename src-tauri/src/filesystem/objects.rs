use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{BundleDigest, OperationId, UtcTimestamp};

use super::{
    BundleCaps, BundleHashError, BundleStats, HashedBundle, MetadataFingerprint,
    durable::{atomic_write, sync_directory},
    hash_bundle, validate_bundle_symlinks,
};

const OBJECT_SCHEMA_VERSION: u32 = 1;
const OBJECT_DIRECTORY: &str = "sha256-bundle-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectManifest {
    pub schema_version: u32,
    pub digest: BundleDigest,
    pub entry_count: u64,
    pub byte_count: u64,
    pub created_at: UtcTimestamp,
}

impl ObjectManifest {
    fn from_bundle(
        digest: BundleDigest,
        stats: BundleStats,
        created_at: UtcTimestamp,
    ) -> Result<Self, ObjectStoreError> {
        Ok(Self {
            schema_version: OBJECT_SCHEMA_VERSION,
            digest,
            entry_count: u64::try_from(stats.entry_count)
                .map_err(|_| ObjectStoreError::EntryCountOverflow)?,
            byte_count: stats.regular_file_bytes,
            created_at,
        })
    }

    fn validate(&self, expected: BundleDigest, stats: BundleStats) -> Result<(), ObjectStoreError> {
        if self.schema_version != OBJECT_SCHEMA_VERSION {
            return Err(ObjectStoreError::UnsupportedManifestSchema {
                found: self.schema_version,
            });
        }
        let entry_count =
            u64::try_from(stats.entry_count).map_err(|_| ObjectStoreError::EntryCountOverflow)?;
        if self.digest != expected
            || self.entry_count != entry_count
            || self.byte_count != stats.regular_file_bytes
        {
            return Err(ObjectStoreError::CorruptExistingObject { digest: expected });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPublication {
    pub manifest: ObjectManifest,
    pub path: PathBuf,
    pub reused: bool,
}

#[derive(Debug, Clone)]
pub struct ObjectStore {
    objects_root: PathBuf,
    staging_root: PathBuf,
    caps: BundleCaps,
}

impl ObjectStore {
    #[must_use]
    pub fn new(objects_root: impl Into<PathBuf>, staging_root: impl Into<PathBuf>) -> Self {
        Self {
            objects_root: objects_root.into(),
            staging_root: staging_root.into(),
            caps: BundleCaps::default(),
        }
    }

    #[must_use]
    pub const fn with_caps(mut self, caps: BundleCaps) -> Self {
        self.caps = caps;
        self
    }

    #[must_use]
    pub fn object_path(&self, digest: BundleDigest) -> PathBuf {
        let encoded = hex::encode(digest.bytes());
        self.objects_root
            .join(OBJECT_DIRECTORY)
            .join(&encoded[..2])
            .join(&encoded[2..])
    }

    /// Copies, verifies, and atomically publishes an immutable Bundle object.
    ///
    /// # Errors
    ///
    /// Returns an error if the source is unsafe or unstable, the expected digest differs,
    /// publication is not durable, or an existing object does not verify.
    pub fn publish(
        &self,
        operation_id: OperationId,
        source: &Path,
        expected_digest: Option<BundleDigest>,
        created_at: UtcTimestamp,
    ) -> Result<ObjectPublication, ObjectStoreError> {
        validate_bundle_symlinks(source, self.caps)?;

        let operation_staging = self
            .staging_root
            .join(operation_id.to_string())
            .join("objects");
        fs::create_dir_all(&operation_staging)?;
        sync_directory(&operation_staging)?;
        let staged_object = operation_staging.join(format!("{}.tmp", Uuid::now_v7()));
        fs::create_dir(&staged_object)?;

        let result = self.publish_staged(source, &staged_object, expected_digest, created_at);
        if staged_object.exists() {
            let _ = fs::remove_dir_all(&staged_object);
            let _ = sync_directory(&operation_staging);
        }
        result
    }

    fn publish_staged(
        &self,
        source: &Path,
        staged_object: &Path,
        expected_digest: Option<BundleDigest>,
        created_at: UtcTimestamp,
    ) -> Result<ObjectPublication, ObjectStoreError> {
        let staged_bundle = staged_object.join("bundle");
        let hashed = copy_bundle_exact(source, &staged_bundle, self.caps)?;
        if expected_digest.is_some_and(|expected| expected != hashed.digest) {
            return Err(ObjectStoreError::DigestMismatch {
                expected: expected_digest.expect("checked as some"),
                actual: hashed.digest,
            });
        }

        let destination = self.object_path(hashed.digest);
        if destination.exists() {
            let manifest = self.verify(hashed.digest)?;
            return Ok(ObjectPublication {
                manifest,
                path: destination,
                reused: true,
            });
        }

        let manifest = ObjectManifest::from_bundle(hashed.digest, hashed.stats, created_at)?;
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        atomic_write(&staged_object.join("object.json"), &bytes)
            .map_err(|source| ObjectStoreError::DurableWrite(source.to_string()))?;
        sync_tree(staged_object).map_err(|source| ObjectStoreError::FilesystemStep {
            step: "flush staged object tree",
            source,
        })?;

        let destination_parent = destination
            .parent()
            .ok_or(ObjectStoreError::InvalidObjectPath)?;
        fs::create_dir_all(destination_parent).map_err(|source| {
            ObjectStoreError::FilesystemStep {
                step: "create object prefix directory",
                source,
            }
        })?;
        sync_directory(destination_parent).map_err(|source| ObjectStoreError::FilesystemStep {
            step: "flush object prefix directory",
            source,
        })?;
        match fs::rename(staged_object, &destination) {
            Ok(()) => sync_directory(destination_parent).map_err(|source| {
                ObjectStoreError::FilesystemStep {
                    step: "flush published object directory",
                    source,
                }
            })?,
            Err(_error) if destination.exists() => {
                let existing = self.verify(hashed.digest)?;
                return Ok(ObjectPublication {
                    manifest: existing,
                    path: destination,
                    reused: true,
                });
            }
            Err(error) => return Err(ObjectStoreError::Io(error)),
        }
        make_tree_read_only(&destination).map_err(|source| ObjectStoreError::FilesystemStep {
            step: "protect published object",
            source,
        })?;
        sync_directory(destination_parent).map_err(|source| ObjectStoreError::FilesystemStep {
            step: "flush protected object directory",
            source,
        })?;

        let verified = self.verify(hashed.digest)?;
        Ok(ObjectPublication {
            manifest: verified,
            path: destination,
            reused: false,
        })
    }

    /// Verifies a published object against both its key and canonical Bundle bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the object is absent, malformed, or content-corrupt.
    pub fn verify(&self, digest: BundleDigest) -> Result<ObjectManifest, ObjectStoreError> {
        let object = self.object_path(digest);
        let metadata = fs::symlink_metadata(&object)
            .map_err(|source| ObjectStoreError::ReadExistingObject { digest, source })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ObjectStoreError::CorruptExistingObject { digest });
        }

        let manifest_bytes = fs::read(object.join("object.json"))
            .map_err(|source| ObjectStoreError::ReadExistingObject { digest, source })?;
        let manifest: ObjectManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|source| ObjectStoreError::InvalidObjectManifest { digest, source })?;
        let hashed = hash_bundle(&object.join("bundle"), self.caps)
            .map_err(|source| ObjectStoreError::InvalidExistingBundle { digest, source })?;
        if hashed.digest != digest {
            return Err(ObjectStoreError::CorruptExistingObject { digest });
        }
        manifest.validate(digest, hashed.stats)?;
        Ok(manifest)
    }
}

/// Copies one validated Bundle without following links, preserving exact bytes, contained
/// symbolic links, empty directories, and semantic executable bits, then re-hashes the copy.
///
/// The destination must not exist. This is the shared source-to-operation-staging primitive used
/// by object publication and takeover staging; it never mutates the source.
///
/// # Errors
///
/// Returns an error when the source is unsafe or unstable, a policy cap is exceeded, the
/// destination already exists, or the copied Bundle cannot be durably written and verified.
pub fn copy_bundle_exact(
    source: &Path,
    destination: &Path,
    caps: BundleCaps,
) -> Result<HashedBundle, ObjectStoreError> {
    validate_bundle_symlinks(source, caps)?;
    fs::create_dir(destination)?;
    let mut budget = CopyBudget::new(caps);
    copy_directory_contents(source, destination, 0, &mut budget)?;
    sync_directory(destination)?;
    hash_bundle(destination, caps).map_err(ObjectStoreError::Bundle)
}

fn copy_directory_contents(
    source: &Path,
    destination: &Path,
    parent_depth: usize,
    budget: &mut CopyBudget,
) -> Result<(), ObjectStoreError> {
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let before = fs::symlink_metadata(&source_path)?;
        let fingerprint = MetadataFingerprint::from_metadata(&before);
        let file_type = before.file_type();
        budget.account(&source_path, parent_depth + 1, &before)?;
        if file_type.is_dir() {
            fs::create_dir(&destination_path)?;
            copy_directory_contents(&source_path, &destination_path, parent_depth + 1, budget)?;
            sync_directory(&destination_path)?;
            set_semantic_permissions(&destination_path, true, false)?;
        } else if file_type.is_file() {
            copy_stable_file(&source_path, &destination_path, fingerprint)?;
        } else if file_type.is_symlink() {
            copy_stable_symlink(&source_path, &destination_path, fingerprint)?;
        } else {
            return Err(ObjectStoreError::UnsupportedSourceEntry { path: source_path });
        }
        let after = fs::symlink_metadata(&source_path)?;
        if MetadataFingerprint::from_metadata(&after) != fingerprint {
            return Err(ObjectStoreError::UnstableSource { path: source_path });
        }
    }
    sync_directory(destination)?;
    Ok(())
}

fn copy_stable_file(
    source: &Path,
    destination: &Path,
    fingerprint: MetadataFingerprint,
) -> Result<(), ObjectStoreError> {
    let input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let maximum_copy = fingerprint.length.saturating_add(1);
    let copied = io::copy(&mut input.take(maximum_copy), &mut output)?;
    if copied != fingerprint.length {
        return Err(ObjectStoreError::UnstableSource {
            path: source.to_path_buf(),
        });
    }
    set_semantic_permissions(destination, false, fingerprint.executable)?;
    output.flush()?;
    output.sync_all()?;
    Ok(())
}

struct CopyBudget {
    caps: BundleCaps,
    entry_count: usize,
    regular_file_bytes: u64,
}

impl CopyBudget {
    const fn new(caps: BundleCaps) -> Self {
        Self {
            caps,
            entry_count: 0,
            regular_file_bytes: 0,
        }
    }

    fn account(
        &mut self,
        path: &Path,
        depth: usize,
        metadata: &fs::Metadata,
    ) -> Result<(), ObjectStoreError> {
        self.entry_count =
            self.entry_count
                .checked_add(1)
                .ok_or_else(|| ObjectStoreError::CopyCapsExceeded {
                    path: path.to_path_buf(),
                })?;
        if self.entry_count > self.caps.maximum_entries || depth > self.caps.maximum_depth {
            return Err(ObjectStoreError::CopyCapsExceeded {
                path: path.to_path_buf(),
            });
        }
        if metadata.is_file() {
            if metadata.len() > self.caps.maximum_single_file_bytes {
                return Err(ObjectStoreError::CopyCapsExceeded {
                    path: path.to_path_buf(),
                });
            }
            self.regular_file_bytes = self
                .regular_file_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| ObjectStoreError::CopyCapsExceeded {
                    path: path.to_path_buf(),
                })?;
            if self.regular_file_bytes > self.caps.maximum_total_file_bytes {
                return Err(ObjectStoreError::CopyCapsExceeded {
                    path: path.to_path_buf(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn copy_stable_symlink(
    source: &Path,
    destination: &Path,
    fingerprint: MetadataFingerprint,
) -> Result<(), ObjectStoreError> {
    use std::os::unix::fs::symlink;

    let target = fs::read_link(source)?;
    symlink(&target, destination)?;
    let after_target = fs::read_link(source)?;
    let after = fs::symlink_metadata(source)?;
    if target != after_target || MetadataFingerprint::from_metadata(&after) != fingerprint {
        return Err(ObjectStoreError::UnstableSource {
            path: source.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn set_semantic_permissions(path: &Path, directory: bool, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if directory || executable {
        0o755
    } else {
        0o644
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn sync_tree(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            sync_tree(&path)?;
        } else if metadata.is_file() {
            File::open(&path)?.sync_all()?;
        }
    }
    sync_directory(root)
}

#[cfg(unix)]
fn make_tree_read_only(root: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut directories = vec![root.to_path_buf()];
    let mut cursor = 0;
    while cursor < directories.len() {
        let directory = directories[cursor].clone();
        cursor += 1;
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                directories.push(path);
            } else if metadata.is_file() {
                let mode = metadata.permissions().mode() & !0o200;
                fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
            }
        }
    }
    for directory in directories.into_iter().rev() {
        let metadata = fs::metadata(&directory)?;
        let mode = metadata.permissions().mode() & !0o200;
        fs::set_permissions(directory, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ObjectStoreError {
    #[error("object store filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("object store failed to {step}: {source}")]
    FilesystemStep {
        step: &'static str,
        source: io::Error,
    },
    #[error("bundle validation failed: {0}")]
    Bundle(#[from] BundleHashError),
    #[error("durable object metadata write failed: {0}")]
    DurableWrite(String),
    #[error("object metadata JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("staged Bundle digest {actual} differs from expected {expected}")]
    DigestMismatch {
        expected: BundleDigest,
        actual: BundleDigest,
    },
    #[error("Bundle entry count cannot be represented by the object manifest")]
    EntryCountOverflow,
    #[error("object destination path is invalid")]
    InvalidObjectPath,
    #[error("source Bundle contains an unsupported entry at {path:?}")]
    UnsupportedSourceEntry { path: PathBuf },
    #[error("source Bundle changed while copying at {path:?}")]
    UnstableSource { path: PathBuf },
    #[error("source Bundle exceeded copy safety caps at {path:?}")]
    CopyCapsExceeded { path: PathBuf },
    #[error("could not read existing object {digest}: {source}")]
    ReadExistingObject {
        digest: BundleDigest,
        source: io::Error,
    },
    #[error("object {digest} has invalid metadata: {source}")]
    InvalidObjectManifest {
        digest: BundleDigest,
        source: serde_json::Error,
    },
    #[error("object {digest} has an invalid Bundle: {source}")]
    InvalidExistingBundle {
        digest: BundleDigest,
        source: BundleHashError,
    },
    #[error("object {digest} does not match its immutable key or metadata")]
    CorruptExistingObject { digest: BundleDigest },
    #[error("unsupported object manifest schema {found}")]
    UnsupportedManifestSchema { found: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(root: &Path) -> ObjectStore {
        ObjectStore::new(root.join("objects"), root.join("staging"))
    }

    fn bundle(root: &Path, body: &str) -> PathBuf {
        let path = root.join(format!("bundle-{}", Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("SKILL.md"), body).unwrap();
        path
    }

    #[test]
    fn publication_is_content_addressed_durable_and_deduplicated() {
        let root = tempfile::tempdir().unwrap();
        let source = bundle(root.path(), "skill\n");
        let store = store(root.path());
        let first = store
            .publish(
                OperationId::generate(),
                &source,
                None,
                UtcTimestamp::from_unix_millis(1_000).unwrap(),
            )
            .unwrap();
        let second = store
            .publish(
                OperationId::generate(),
                &source,
                Some(first.manifest.digest),
                UtcTimestamp::from_unix_millis(2_000).unwrap(),
            )
            .unwrap();

        assert!(!first.reused);
        assert!(second.reused);
        assert_eq!(first.path, second.path);
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(store.verify(first.manifest.digest).unwrap(), first.manifest);
        assert!(first.path.join("object.json").is_file());
        assert!(first.path.join("bundle/SKILL.md").is_file());
    }

    #[test]
    fn corrupt_existing_object_is_rejected_instead_of_reused() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let source = bundle(root.path(), "original\n");
        let store = store(root.path());
        let publication = store
            .publish(
                OperationId::generate(),
                &source,
                None,
                UtcTimestamp::from_unix_millis(1_000).unwrap(),
            )
            .unwrap();

        let object_file = publication.path.join("bundle/SKILL.md");
        fs::set_permissions(&object_file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&object_file, "corrupt\n").unwrap();

        assert!(matches!(
            store.publish(
                OperationId::generate(),
                &source,
                Some(publication.manifest.digest),
                UtcTimestamp::from_unix_millis(2_000).unwrap(),
            ),
            Err(ObjectStoreError::CorruptExistingObject { .. }
                | ObjectStoreError::InvalidExistingBundle { .. })
        ));
    }

    #[test]
    fn digest_mismatch_never_publishes_an_object() {
        let root = tempfile::tempdir().unwrap();
        let source = bundle(root.path(), "skill\n");
        let store = store(root.path());
        let expected = BundleDigest::from_bytes([9; 32]);

        assert!(matches!(
            store.publish(
                OperationId::generate(),
                &source,
                Some(expected),
                UtcTimestamp::from_unix_millis(1_000).unwrap(),
            ),
            Err(ObjectStoreError::DigestMismatch { .. })
        ));
        assert!(!store.object_path(expected).exists());
    }

    #[test]
    fn interrupted_operation_staging_is_never_mistaken_for_a_published_object() {
        let root = tempfile::tempdir().unwrap();
        let source = bundle(root.path(), "skill\n");
        let store = store(root.path());
        let operation_id = OperationId::generate();
        let interrupted = root
            .path()
            .join("staging")
            .join(operation_id.to_string())
            .join("objects/interrupted.tmp");
        fs::create_dir_all(&interrupted).unwrap();
        fs::write(interrupted.join("object.json"), b"partial").unwrap();

        let publication = store
            .publish(
                operation_id,
                &source,
                None,
                UtcTimestamp::from_unix_millis(1_000).unwrap(),
            )
            .unwrap();

        assert!(interrupted.exists());
        assert!(publication.path.starts_with(root.path().join("objects")));
        assert_eq!(
            store.verify(publication.manifest.digest).unwrap(),
            publication.manifest
        );
    }

    #[test]
    fn copy_enforces_caps_independently_of_the_validation_pass() {
        let root = tempfile::tempdir().unwrap();
        let source = bundle(root.path(), "larger than cap\n");
        let destination = root.path().join("copy");
        let caps = BundleCaps {
            maximum_single_file_bytes: 1,
            ..BundleCaps::default()
        };
        fs::create_dir(&destination).unwrap();
        let mut budget = CopyBudget::new(caps);

        assert!(matches!(
            copy_directory_contents(&source, &destination, 0, &mut budget),
            Err(ObjectStoreError::CopyCapsExceeded { .. })
        ));
    }
}
