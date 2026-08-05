use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::domain::{BundleDigest, BundleRelativePath, NameError};

use super::MetadataFingerprint;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

const HASH_DOMAIN: &[u8] = b"skills-hub-bundle\0v1\0";
const ENTRY_DIRECTORY: u8 = 1;
const ENTRY_REGULAR_FILE: u8 = 2;
const ENTRY_SYMLINK: u8 = 3;
const MODE_DIRECTORY: u8 = 1;
const MODE_REGULAR: u8 = 2;
const MODE_EXECUTABLE: u8 = 3;
const MODE_SYMLINK: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleCaps {
    pub maximum_depth: usize,
    pub maximum_entries: usize,
    pub maximum_total_file_bytes: u64,
    pub maximum_single_file_bytes: u64,
}

impl Default for BundleCaps {
    fn default() -> Self {
        Self {
            maximum_depth: 64,
            maximum_entries: 10_000,
            maximum_total_file_bytes: 1024 * 1024 * 1024,
            maximum_single_file_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleStats {
    pub maximum_depth: usize,
    pub entry_count: usize,
    pub regular_file_bytes: u64,
    pub largest_file_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashedBundle {
    pub digest: BundleDigest,
    pub stats: BundleStats,
}

#[derive(Debug)]
struct EntryDescriptor {
    path: PathBuf,
    relative: BundleRelativePath,
    fingerprint: MetadataFingerprint,
    kind: BundleEntryKind,
    link_target: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BundleEntryKind {
    Directory,
    RegularFile,
    Symlink,
}

/// Validates and hashes a Bundle using the `sha256-bundle-v1` protocol.
///
/// # Errors
///
/// Returns [`BundleHashError`] when the tree is invalid, unstable, unreadable, or over policy caps.
pub fn hash_bundle(root: &Path, caps: BundleCaps) -> Result<HashedBundle, BundleHashError> {
    let (entries, stats) = collect_entries(root, caps)?;
    validate_skill_manifest(root)?;

    let mut hasher = Sha256::new();
    hasher.update(HASH_DOMAIN);
    for entry in entries.values() {
        encode_entry(&mut hasher, entry)?;
    }
    Ok(HashedBundle {
        digest: BundleDigest::from_bytes(hasher.finalize().into()),
        stats,
    })
}

fn collect_entries(
    root: &Path,
    caps: BundleCaps,
) -> Result<(BTreeMap<Vec<u8>, EntryDescriptor>, BundleStats), BundleHashError> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|source| BundleHashError::ReadRoot { source })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(BundleHashError::RootNotDirectory);
    }

    let mut entries = BTreeMap::new();
    let mut stats = BundleStats::default();
    visit_directory(root, root, caps, &mut stats, &mut entries)?;
    for entry in entries.values_mut() {
        if entry.kind == BundleEntryKind::Symlink {
            entry.link_target = Some(read_stable_link_target(entry)?);
        }
    }
    Ok((entries, stats))
}

fn visit_directory(
    root: &Path,
    directory: &Path,
    caps: BundleCaps,
    stats: &mut BundleStats,
    entries: &mut BTreeMap<Vec<u8>, EntryDescriptor>,
) -> Result<(), BundleHashError> {
    let children = fs::read_dir(directory).map_err(|source| BundleHashError::ReadDirectory {
        relative: display_relative(root, directory),
        source,
    })?;

    for child in children {
        let child = child.map_err(|source| BundleHashError::ReadDirectory {
            relative: display_relative(root, directory),
            source,
        })?;
        let path = child.path();
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_| BundleHashError::PathEscaped)?;
        let relative = BundleRelativePath::from_path(relative_path).map_err(|source| {
            BundleHashError::InvalidName {
                relative: display_relative(root, &path),
                source,
            }
        })?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| BundleHashError::ReadEntry {
                relative: relative.clone(),
                source,
            })?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_dir() {
            BundleEntryKind::Directory
        } else if file_type.is_file() {
            BundleEntryKind::RegularFile
        } else if file_type.is_symlink() {
            BundleEntryKind::Symlink
        } else {
            return Err(BundleHashError::UnsupportedEntry {
                relative: relative.clone(),
            });
        };

        stats.entry_count = stats
            .entry_count
            .checked_add(1)
            .ok_or(BundleHashError::EntryLimitExceeded)?;
        if stats.entry_count > caps.maximum_entries {
            return Err(BundleHashError::EntryLimitExceeded);
        }
        stats.maximum_depth = stats.maximum_depth.max(relative.depth());
        if stats.maximum_depth > caps.maximum_depth {
            return Err(BundleHashError::DepthLimitExceeded {
                relative: relative.clone(),
            });
        }
        if kind == BundleEntryKind::RegularFile {
            if metadata.len() > caps.maximum_single_file_bytes {
                return Err(BundleHashError::SingleFileLimitExceeded {
                    relative: relative.clone(),
                    size: metadata.len(),
                });
            }
            stats.regular_file_bytes = stats
                .regular_file_bytes
                .checked_add(metadata.len())
                .ok_or(BundleHashError::TotalFileLimitExceeded)?;
            if stats.regular_file_bytes > caps.maximum_total_file_bytes {
                return Err(BundleHashError::TotalFileLimitExceeded);
            }
            stats.largest_file_bytes = stats.largest_file_bytes.max(metadata.len());
        }

        let key = relative.as_str().as_bytes().to_vec();
        let descriptor = EntryDescriptor {
            path: path.clone(),
            relative: relative.clone(),
            fingerprint: MetadataFingerprint::from_metadata(&metadata),
            kind,
            link_target: None,
        };
        if entries.insert(key, descriptor).is_some() {
            return Err(BundleHashError::NormalizedNameCollision { relative });
        }
        if kind == BundleEntryKind::Directory {
            visit_directory(root, &path, caps, stats, entries)?;
        }
    }
    Ok(())
}

fn validate_skill_manifest(root: &Path) -> Result<(), BundleHashError> {
    let path = root.join("SKILL.md");
    let metadata = fs::symlink_metadata(&path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            BundleHashError::MissingSkillManifest
        } else {
            BundleHashError::ReadManifest { source }
        }
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(BundleHashError::InvalidSkillManifestType);
    }
    let bytes = fs::read(path).map_err(|source| BundleHashError::ReadManifest { source })?;
    std::str::from_utf8(&bytes).map_err(|_| BundleHashError::SkillManifestNotUtf8)?;
    Ok(())
}

fn encode_entry(hasher: &mut Sha256, entry: &EntryDescriptor) -> Result<(), BundleHashError> {
    let (entry_type, mode, payload) = match entry.kind {
        BundleEntryKind::Directory => (ENTRY_DIRECTORY, MODE_DIRECTORY, Vec::new()),
        BundleEntryKind::RegularFile => {
            let bytes = read_stable_file(entry)?;
            (
                ENTRY_REGULAR_FILE,
                if entry.fingerprint.executable {
                    MODE_EXECUTABLE
                } else {
                    MODE_REGULAR
                },
                bytes,
            )
        }
        BundleEntryKind::Symlink => {
            let target = read_stable_link_target(entry)?;
            if entry.link_target.as_ref() != Some(&target) {
                return Err(BundleHashError::UnstableInput {
                    relative: entry.relative.clone(),
                });
            }
            (ENTRY_SYMLINK, MODE_SYMLINK, os_path_bytes(&target).to_vec())
        }
    };
    let path = entry.relative.as_str().as_bytes();
    hasher.update([entry_type]);
    hasher.update(length_bytes(path.len())?);
    hasher.update(path);
    hasher.update([mode]);
    hasher.update(length_bytes(payload.len())?);
    hasher.update(payload);
    Ok(())
}

fn read_stable_file(entry: &EntryDescriptor) -> Result<Vec<u8>, BundleHashError> {
    let mut file = File::open(&entry.path).map_err(|source| BundleHashError::ReadEntry {
        relative: entry.relative.clone(),
        source,
    })?;
    let open_metadata = file
        .metadata()
        .map_err(|source| BundleHashError::ReadEntry {
            relative: entry.relative.clone(),
            source,
        })?;
    let open_fingerprint = MetadataFingerprint::from_metadata(&open_metadata);
    if open_fingerprint != entry.fingerprint {
        return Err(BundleHashError::UnstableInput {
            relative: entry.relative.clone(),
        });
    }

    let capacity = usize::try_from(entry.fingerprint.length).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|source| BundleHashError::ReadEntry {
            relative: entry.relative.clone(),
            source,
        })?;
    let final_metadata = file
        .metadata()
        .map_err(|source| BundleHashError::ReadEntry {
            relative: entry.relative.clone(),
            source,
        })?;
    if MetadataFingerprint::from_metadata(&final_metadata) != entry.fingerprint
        || u64::try_from(bytes.len()).ok() != Some(entry.fingerprint.length)
    {
        return Err(BundleHashError::UnstableInput {
            relative: entry.relative.clone(),
        });
    }
    Ok(bytes)
}

fn read_stable_link_target(entry: &EntryDescriptor) -> Result<PathBuf, BundleHashError> {
    let before =
        fs::symlink_metadata(&entry.path).map_err(|source| BundleHashError::ReadEntry {
            relative: entry.relative.clone(),
            source,
        })?;
    if MetadataFingerprint::from_metadata(&before) != entry.fingerprint {
        return Err(BundleHashError::UnstableInput {
            relative: entry.relative.clone(),
        });
    }
    let target = fs::read_link(&entry.path).map_err(|source| BundleHashError::ReadEntry {
        relative: entry.relative.clone(),
        source,
    })?;
    let after = fs::symlink_metadata(&entry.path).map_err(|source| BundleHashError::ReadEntry {
        relative: entry.relative.clone(),
        source,
    })?;
    if MetadataFingerprint::from_metadata(&after) != entry.fingerprint {
        return Err(BundleHashError::UnstableInput {
            relative: entry.relative.clone(),
        });
    }
    Ok(target)
}

fn length_bytes(length: usize) -> Result<[u8; 8], BundleHashError> {
    u64::try_from(length)
        .map(u64::to_be_bytes)
        .map_err(|_| BundleHashError::LengthOverflow)
}

#[cfg(unix)]
fn os_path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

/// Verifies that every internal Bundle symlink is relative, contained, resolvable, and acyclic.
///
/// # Errors
///
/// Returns [`BundleHashError`] for an invalid Bundle or unsafe link.
pub fn validate_bundle_symlinks(root: &Path, caps: BundleCaps) -> Result<(), BundleHashError> {
    let (entries, _) = collect_entries(root, caps)?;
    validate_skill_manifest(root)?;
    let root_canonical = root
        .canonicalize()
        .map_err(|source| BundleHashError::ReadRoot { source })?;
    let mut targets = BTreeMap::new();

    for entry in entries.values() {
        if entry.kind != BundleEntryKind::Symlink {
            continue;
        }
        let raw_target =
            entry
                .link_target
                .as_ref()
                .ok_or_else(|| BundleHashError::UnstableInput {
                    relative: entry.relative.clone(),
                })?;
        let resolved = resolve_link_lexically(&entry.relative, raw_target)?;
        targets.insert(entry.relative.clone(), resolved);
    }

    for start in targets.keys() {
        let mut seen = BTreeSet::new();
        let mut current = start;
        while let Some(next) = targets.get(current) {
            if !seen.insert(current.clone()) {
                return Err(BundleHashError::CyclicSymlink {
                    relative: start.clone(),
                });
            }
            current = next;
        }
    }

    for entry in entries.values() {
        if entry.kind != BundleEntryKind::Symlink {
            continue;
        }
        let canonical =
            entry
                .path
                .canonicalize()
                .map_err(|source| BundleHashError::BrokenOrCyclicSymlink {
                    relative: entry.relative.clone(),
                    source,
                })?;
        if !canonical.starts_with(&root_canonical) {
            return Err(BundleHashError::EscapingSymlink {
                relative: entry.relative.clone(),
            });
        }
        if entry.link_target.as_ref() != Some(&read_stable_link_target(entry)?) {
            return Err(BundleHashError::UnstableInput {
                relative: entry.relative.clone(),
            });
        }
    }
    Ok(())
}

fn resolve_link_lexically(
    link: &BundleRelativePath,
    raw_target: &Path,
) -> Result<BundleRelativePath, BundleHashError> {
    if raw_target.is_absolute() {
        return Err(BundleHashError::AbsoluteSymlink {
            relative: link.clone(),
        });
    }
    let mut resolved: Vec<String> = link.as_str().split('/').map(str::to_owned).collect();
    resolved.pop();
    for component in raw_target.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if resolved.pop().is_none() {
                    return Err(BundleHashError::EscapingSymlink {
                        relative: link.clone(),
                    });
                }
            }
            Component::Normal(component) => {
                let component = component
                    .to_str()
                    .ok_or_else(|| BundleHashError::InvalidName {
                        relative: link.to_string(),
                        source: NameError::UnsupportedName,
                    })?;
                resolved.push(component.to_owned());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(BundleHashError::AbsoluteSymlink {
                    relative: link.clone(),
                });
            }
        }
    }
    BundleRelativePath::parse(&resolved.join("/")).map_err(|source| BundleHashError::InvalidName {
        relative: link.to_string(),
        source,
    })
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(".")
        .to_owned()
}

#[derive(Debug, Error)]
pub enum BundleHashError {
    #[error("could not read bundle root: {source}")]
    ReadRoot { source: io::Error },
    #[error("bundle root must be a directory, not a symlink")]
    RootNotDirectory,
    #[error("could not read bundle directory '{relative}': {source}")]
    ReadDirectory { relative: String, source: io::Error },
    #[error("could not inspect bundle entry '{relative}': {source}")]
    ReadEntry {
        relative: BundleRelativePath,
        source: io::Error,
    },
    #[error("bundle entry has an unsupported name at '{relative}': {source}")]
    InvalidName { relative: String, source: NameError },
    #[error("bundle entry path escaped its root")]
    PathEscaped,
    #[error("bundle has an unsupported entry type at '{relative}'")]
    UnsupportedEntry { relative: BundleRelativePath },
    #[error("bundle has two paths that normalize to '{relative}'")]
    NormalizedNameCollision { relative: BundleRelativePath },
    #[error("bundle exceeds the entry-count limit")]
    EntryLimitExceeded,
    #[error("bundle exceeds the depth limit at '{relative}'")]
    DepthLimitExceeded { relative: BundleRelativePath },
    #[error("bundle file '{relative}' is too large ({size} bytes)")]
    SingleFileLimitExceeded {
        relative: BundleRelativePath,
        size: u64,
    },
    #[error("bundle exceeds the total regular-file byte limit")]
    TotalFileLimitExceeded,
    #[error("bundle does not contain a direct SKILL.md file")]
    MissingSkillManifest,
    #[error("bundle SKILL.md must be a regular file")]
    InvalidSkillManifestType,
    #[error("bundle SKILL.md is not valid UTF-8")]
    SkillManifestNotUtf8,
    #[error("could not read bundle SKILL.md: {source}")]
    ReadManifest { source: io::Error },
    #[error("bundle changed while it was being read at '{relative}'")]
    UnstableInput { relative: BundleRelativePath },
    #[error("bundle entry length cannot be represented by the hashing protocol")]
    LengthOverflow,
    #[error("bundle link '{relative}' is absolute")]
    AbsoluteSymlink { relative: BundleRelativePath },
    #[error("bundle link '{relative}' escapes the bundle")]
    EscapingSymlink { relative: BundleRelativePath },
    #[error("bundle link '{relative}' is broken or cyclic: {source}")]
    BrokenOrCyclicSymlink {
        relative: BundleRelativePath,
        source: io::Error,
    },
    #[error("bundle link cycle starts at '{relative}'")]
    CyclicSymlink { relative: BundleRelativePath },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_skill(root: &Path, body: &str) {
        fs::write(root.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn golden_empty_skill_bundle() {
        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "---\nname: golden\n---\n");

        let hashed = hash_bundle(bundle.path(), BundleCaps::default()).unwrap();

        assert_eq!(
            hashed.digest.to_string(),
            "sha256-bundle-v1:92bfb918e30f2a32bdc0a79096a6f99143fa3ffa4f56cf4a5db0da8fcce05515"
        );
        assert_eq!(
            hashed.stats,
            BundleStats {
                maximum_depth: 1,
                entry_count: 1,
                regular_file_bytes: 21,
                largest_file_bytes: 21,
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn golden_composite_bundle_covers_all_entry_and_mode_classes() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill\n");
        fs::write(bundle.path().join(".hidden"), [0, 255, 1]).unwrap();
        fs::create_dir(bundle.path().join("empty")).unwrap();
        let script = bundle.path().join("script.sh");
        fs::write(&script, "echo hi\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("script.sh", bundle.path().join("link")).unwrap();

        assert_eq!(
            hash_bundle(bundle.path(), BundleCaps::default())
                .unwrap()
                .digest
                .to_string(),
            "sha256-bundle-v1:2f2e92ed3bffc30a762589f17114f874fb7279bb1bb7617532e17f1670011db8"
        );
    }

    #[test]
    fn hash_is_creation_order_independent_and_tracks_hidden_and_empty_entries() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        for root in [first.path(), second.path()] {
            write_skill(root, "skill");
        }
        fs::create_dir(first.path().join("empty")).unwrap();
        fs::write(first.path().join(".hidden"), b"value").unwrap();
        fs::write(second.path().join(".hidden"), b"value").unwrap();
        fs::create_dir(second.path().join("empty")).unwrap();

        let first_hash = hash_bundle(first.path(), BundleCaps::default()).unwrap();
        let second_hash = hash_bundle(second.path(), BundleCaps::default()).unwrap();
        assert_eq!(first_hash, second_hash);

        fs::remove_dir(first.path().join("empty")).unwrap();
        assert_ne!(
            hash_bundle(first.path(), BundleCaps::default())
                .unwrap()
                .digest,
            second_hash.digest
        );
    }

    #[test]
    fn file_bytes_and_relative_path_change_the_digest_but_mtime_does_not() {
        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill");
        let file = bundle.path().join("one.txt");
        fs::write(&file, "one").unwrap();
        let original = hash_bundle(bundle.path(), BundleCaps::default())
            .unwrap()
            .digest;

        fs::write(&file, "one").unwrap();
        let mtime_only = hash_bundle(bundle.path(), BundleCaps::default())
            .unwrap()
            .digest;
        assert_eq!(original, mtime_only);

        fs::write(&file, "two").unwrap();
        let changed_bytes = hash_bundle(bundle.path(), BundleCaps::default())
            .unwrap()
            .digest;
        assert_ne!(original, changed_bytes);

        fs::rename(&file, bundle.path().join("two.txt")).unwrap();
        let changed_path = hash_bundle(bundle.path(), BundleCaps::default())
            .unwrap()
            .digest;
        assert_ne!(changed_bytes, changed_path);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_and_symlink_target_change_the_digest() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill");
        let script = bundle.path().join("run.sh");
        fs::write(&script, "echo safe").unwrap();
        symlink("run.sh", bundle.path().join("run-link")).unwrap();
        let regular = hash_bundle(bundle.path(), BundleCaps::default())
            .unwrap()
            .digest;

        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let executable = hash_bundle(bundle.path(), BundleCaps::default())
            .unwrap()
            .digest;
        assert_ne!(regular, executable);

        fs::remove_file(bundle.path().join("run-link")).unwrap();
        symlink("SKILL.md", bundle.path().join("run-link")).unwrap();
        let changed_link = hash_bundle(bundle.path(), BundleCaps::default())
            .unwrap()
            .digest;
        assert_ne!(executable, changed_link);
    }

    #[cfg(unix)]
    #[test]
    fn safe_link_validation_rejects_absolute_escaping_and_broken_links() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill");
        symlink("/tmp", bundle.path().join("absolute")).unwrap();
        assert!(matches!(
            validate_bundle_symlinks(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::AbsoluteSymlink { .. })
        ));
        fs::remove_file(bundle.path().join("absolute")).unwrap();

        symlink("../outside", bundle.path().join("escaping")).unwrap();
        assert!(matches!(
            validate_bundle_symlinks(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::EscapingSymlink { .. })
        ));
        fs::remove_file(bundle.path().join("escaping")).unwrap();

        symlink("missing", bundle.path().join("broken")).unwrap();
        assert!(matches!(
            validate_bundle_symlinks(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::BrokenOrCyclicSymlink { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn safe_link_validation_identifies_cycles_before_canonical_resolution() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill");
        symlink("second", bundle.path().join("first")).unwrap();
        symlink("first", bundle.path().join("second")).unwrap();
        assert!(matches!(
            validate_bundle_symlinks(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::CyclicSymlink { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn stable_read_rejects_a_symlink_changed_after_collection() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill");
        let link = bundle.path().join("link");
        symlink("SKILL.md", &link).unwrap();
        let (entries, _) = collect_entries(bundle.path(), BundleCaps::default()).unwrap();
        let entry = entries
            .values()
            .find(|entry| entry.kind == BundleEntryKind::Symlink)
            .unwrap();
        fs::remove_file(&link).unwrap();
        symlink("missing", &link).unwrap();

        assert!(matches!(
            read_stable_link_target(entry),
            Err(BundleHashError::UnstableInput { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn link_validation_uses_the_callers_active_caps() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill");
        symlink("SKILL.md", bundle.path().join("link")).unwrap();
        let caps = BundleCaps {
            maximum_entries: 1,
            ..BundleCaps::default()
        };

        assert!(matches!(
            validate_bundle_symlinks(bundle.path(), caps),
            Err(BundleHashError::EntryLimitExceeded)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_special_file_entries() {
        use std::os::unix::net::UnixListener;

        let bundle = tempdir().unwrap();
        write_skill(bundle.path(), "skill");
        let _socket = UnixListener::bind(bundle.path().join("socket")).unwrap();
        assert!(matches!(
            hash_bundle(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::UnsupportedEntry { .. })
        ));
    }

    #[test]
    fn stable_read_rejects_a_file_changed_after_metadata_collection() {
        let bundle = tempdir().unwrap();
        let path = bundle.path().join("file.txt");
        fs::write(&path, "before").unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let entry = EntryDescriptor {
            path: path.clone(),
            relative: BundleRelativePath::parse("file.txt").unwrap(),
            fingerprint: MetadataFingerprint::from_metadata(&metadata),
            kind: BundleEntryKind::RegularFile,
            link_target: None,
        };
        fs::write(path, "after with a different length").unwrap();

        assert!(matches!(
            read_stable_file(&entry),
            Err(BundleHashError::UnstableInput { .. })
        ));
    }

    #[test]
    fn validates_manifest_encoding_and_caps_before_hashing() {
        let bundle = tempdir().unwrap();
        assert!(matches!(
            hash_bundle(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::MissingSkillManifest)
        ));
        fs::write(bundle.path().join("SKILL.md"), [0xff]).unwrap();
        assert!(matches!(
            hash_bundle(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::SkillManifestNotUtf8)
        ));

        write_skill(bundle.path(), "too much");
        let caps = BundleCaps {
            maximum_single_file_bytes: 2,
            ..BundleCaps::default()
        };
        assert!(matches!(
            hash_bundle(bundle.path(), caps),
            Err(BundleHashError::SingleFileLimitExceeded { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn skill_manifest_cannot_be_a_symlink() {
        use std::os::unix::fs::symlink;

        let bundle = tempdir().unwrap();
        fs::write(bundle.path().join("real.md"), "skill").unwrap();
        symlink("real.md", bundle.path().join("SKILL.md")).unwrap();
        assert!(matches!(
            hash_bundle(bundle.path(), BundleCaps::default()),
            Err(BundleHashError::InvalidSkillManifestType)
        ));
    }
}
