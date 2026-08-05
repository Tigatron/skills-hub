use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::BundleRelativePath;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[derive(Debug, Clone)]
pub struct AuthorizedRoot {
    selected: PathBuf,
    canonical: PathBuf,
    identity: MetadataFingerprint,
}

impl AuthorizedRoot {
    /// Opens an existing absolute directory as a canonical authorization boundary.
    ///
    /// # Errors
    ///
    /// Returns [`PathPolicyError`] when the root is relative, missing, unreadable, or not a directory.
    pub fn open(selected: impl Into<PathBuf>) -> Result<Self, PathPolicyError> {
        let selected = selected.into();
        if !selected.is_absolute() {
            return Err(PathPolicyError::RootNotAbsolute);
        }
        let metadata = fs::metadata(&selected).map_err(PathPolicyError::RootUnavailable)?;
        if !metadata.is_dir() {
            return Err(PathPolicyError::RootNotDirectory);
        }
        let canonical = selected
            .canonicalize()
            .map_err(PathPolicyError::RootUnavailable)?;
        let identity = MetadataFingerprint::from_metadata(&metadata);
        Ok(Self {
            selected,
            canonical,
            identity,
        })
    }

    #[must_use]
    pub fn selected_path(&self) -> &Path {
        &self.selected
    }

    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    #[must_use]
    pub const fn identity(&self) -> MetadataFingerprint {
        self.identity
    }

    /// Derives a contained candidate from a validated relative path.
    ///
    /// # Errors
    ///
    /// Returns [`PathPolicyError`] when an ancestor is unsafe, unreadable, or escapes this root.
    pub fn authorize(
        &self,
        relative: &BundleRelativePath,
    ) -> Result<AuthorizedPath, PathPolicyError> {
        let mut nearest = self.canonical.clone();
        let mut missing = Vec::new();
        let components: Vec<_> = relative.as_str().split('/').collect();

        for (index, component) in components.iter().enumerate() {
            if !missing.is_empty() {
                missing.push(*component);
                continue;
            }

            let candidate = nearest.join(component);
            match fs::symlink_metadata(&candidate) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        if index + 1 == components.len() {
                            let nearest_canonical = nearest
                                .canonicalize()
                                .map_err(|source| PathPolicyError::InspectFailed { source })?;
                            if !nearest_canonical.starts_with(&self.canonical) {
                                return Err(PathPolicyError::EscapesRoot);
                            }
                            let nearest_metadata = fs::metadata(&nearest_canonical)
                                .map_err(|source| PathPolicyError::InspectFailed { source })?;
                            return Ok(AuthorizedPath {
                                candidate: nearest_canonical.join(component),
                                relative: relative.clone(),
                                nearest_existing_ancestor: nearest_canonical,
                                ancestor_fingerprint: MetadataFingerprint::from_metadata(
                                    &nearest_metadata,
                                ),
                            });
                        }
                        return Err(PathPolicyError::UnexpectedSymlink {
                            relative: relative.clone(),
                        });
                    }
                    if index + 1 < components.len() && !metadata.is_dir() {
                        return Err(PathPolicyError::NonDirectoryAncestor {
                            relative: relative.clone(),
                        });
                    }
                    nearest = candidate;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(*component);
                }
                Err(source) => return Err(PathPolicyError::InspectFailed { source }),
            }
        }

        let nearest_canonical = nearest
            .canonicalize()
            .map_err(|source| PathPolicyError::InspectFailed { source })?;
        if !nearest_canonical.starts_with(&self.canonical) {
            return Err(PathPolicyError::EscapesRoot);
        }
        let nearest_metadata = fs::metadata(&nearest_canonical)
            .map_err(|source| PathPolicyError::InspectFailed { source })?;
        let mut candidate = nearest_canonical.clone();
        for component in missing {
            candidate.push(component);
        }
        if !candidate.starts_with(&self.canonical) {
            return Err(PathPolicyError::EscapesRoot);
        }

        Ok(AuthorizedPath {
            candidate,
            relative: relative.clone(),
            nearest_existing_ancestor: nearest_canonical,
            ancestor_fingerprint: MetadataFingerprint::from_metadata(&nearest_metadata),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedPath {
    candidate: PathBuf,
    relative: BundleRelativePath,
    nearest_existing_ancestor: PathBuf,
    ancestor_fingerprint: MetadataFingerprint,
}

impl AuthorizedPath {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.candidate
    }

    #[must_use]
    pub fn relative(&self) -> &BundleRelativePath {
        &self.relative
    }

    #[must_use]
    pub fn nearest_existing_ancestor(&self) -> &Path {
        &self.nearest_existing_ancestor
    }

    #[must_use]
    pub const fn ancestor_fingerprint(&self) -> MetadataFingerprint {
        self.ancestor_fingerprint
    }

    /// Captures the stable identity of the candidate's immediate parent directory without
    /// following a symbolic link at that parent.
    ///
    /// # Errors
    ///
    /// Returns [`PathPolicyError`] when the final parent is missing, unreadable, a symbolic link,
    /// or not a directory. Operation staging requires this exact parent to already exist.
    pub fn parent_identity(&self) -> Result<PathIdentity, PathPolicyError> {
        let parent = self
            .candidate
            .parent()
            .ok_or(PathPolicyError::FinalParentUnavailable)?;
        let metadata = fs::symlink_metadata(parent)
            .map_err(|source| PathPolicyError::InspectFailed { source })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PathPolicyError::FinalParentNotDirectory);
        }
        Ok(PathIdentity::from_metadata(&metadata))
    }

    /// Captures the candidate entry without following a terminal symbolic link.
    ///
    /// # Errors
    ///
    /// Returns [`PathPolicyError`] when entry metadata or a link target cannot be read.
    pub fn inspect(&self) -> Result<PathObservation, PathPolicyError> {
        let metadata = match fs::symlink_metadata(&self.candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PathObservation {
                    kind: EntryKind::Absent,
                    metadata: None,
                    raw_symlink_target: None,
                });
            }
            Err(source) => return Err(PathPolicyError::InspectFailed { source }),
        };
        let kind = entry_kind(metadata.file_type());
        let raw_symlink_target = if kind == EntryKind::Symlink {
            Some(
                fs::read_link(&self.candidate)
                    .map_err(|source| PathPolicyError::InspectFailed { source })?,
            )
        } else {
            None
        };
        Ok(PathObservation {
            kind,
            metadata: Some(MetadataFingerprint::from_metadata(&metadata)),
            raw_symlink_target,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathObservation {
    pub kind: EntryKind,
    pub metadata: Option<MetadataFingerprint>,
    pub raw_symlink_target: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Absent,
    File,
    Directory,
    Symlink,
    Unsupported,
}

/// Stable inode identity used to detect parent-directory replacement without treating normal
/// directory metadata changes caused by sibling renames as replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathIdentity {
    pub device_id: u64,
    pub file_id: u64,
    pub kind: EntryKind,
}

impl PathIdentity {
    #[cfg(unix)]
    #[must_use]
    pub fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device_id: metadata.dev(),
            file_id: metadata.ino(),
            kind: entry_kind(metadata.file_type()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataFingerprint {
    pub device_id: u64,
    pub file_id: u64,
    pub length: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
    pub kind: EntryKind,
    pub executable: bool,
}

impl MetadataFingerprint {
    #[cfg(unix)]
    #[must_use]
    pub fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device_id: metadata.dev(),
            file_id: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            kind: entry_kind(metadata.file_type()),
            executable: metadata.mode() & 0o111 != 0,
        }
    }
}

fn entry_kind(file_type: fs::FileType) -> EntryKind {
    if file_type.is_file() {
        EntryKind::File
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::Unsupported
    }
}

#[derive(Debug, Error)]
pub enum PathPolicyError {
    #[error("authorized root must be an absolute path")]
    RootNotAbsolute,
    #[error("authorized root is unavailable: {0}")]
    RootUnavailable(std::io::Error),
    #[error("authorized root is not a directory")]
    RootNotDirectory,
    #[error("path contains an unexpected symbolic link: {relative}")]
    UnexpectedSymlink { relative: BundleRelativePath },
    #[error("path contains a non-directory ancestor: {relative}")]
    NonDirectoryAncestor { relative: BundleRelativePath },
    #[error("authorized final path has no parent directory")]
    FinalParentUnavailable,
    #[error("authorized final parent is a symbolic link or not a directory")]
    FinalParentNotDirectory,
    #[error("path inspection failed: {source}")]
    InspectFailed { source: std::io::Error },
    #[error("path escapes the authorized root")]
    EscapesRoot,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::tempdir;

    #[test]
    fn authorizes_nonexistent_descendants_from_nearest_existing_ancestor() {
        let root_dir = tempdir().unwrap();
        fs::create_dir(root_dir.path().join("existing")).unwrap();
        let root = AuthorizedRoot::open(root_dir.path()).unwrap();
        let relative = BundleRelativePath::parse("existing/new/skill").unwrap();
        let authorized = root.authorize(&relative).unwrap();

        assert_eq!(
            authorized.path(),
            root_dir
                .path()
                .canonicalize()
                .unwrap()
                .join("existing/new/skill")
        );
        assert_eq!(
            authorized.nearest_existing_ancestor(),
            root_dir.path().canonicalize().unwrap().join("existing")
        );
        assert!(matches!(
            authorized.parent_identity(),
            Err(PathPolicyError::InspectFailed { source })
                if source.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn captures_only_the_existing_final_parent_identity() {
        let root_dir = tempdir().unwrap();
        let parent = root_dir.path().join("existing");
        fs::create_dir(&parent).unwrap();
        let root = AuthorizedRoot::open(root_dir.path()).unwrap();
        let relative = BundleRelativePath::parse("existing/new").unwrap();
        let authorized = root.authorize(&relative).unwrap();

        let identity = authorized.parent_identity().unwrap();
        let metadata = fs::symlink_metadata(&parent).unwrap();
        assert_eq!(identity, PathIdentity::from_metadata(&metadata));
        assert_eq!(identity.kind, EntryKind::Directory);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_ancestors_even_when_they_resolve_inside_root() {
        use std::os::unix::fs::symlink;

        let root_dir = tempdir().unwrap();
        fs::create_dir(root_dir.path().join("real")).unwrap();
        symlink("real", root_dir.path().join("linked")).unwrap();
        let root = AuthorizedRoot::open(root_dir.path()).unwrap();
        let relative = BundleRelativePath::parse("linked/new").unwrap();

        assert!(matches!(
            root.authorize(&relative),
            Err(PathPolicyError::UnexpectedSymlink { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn authorizes_a_terminal_symlink_without_following_its_target() {
        use std::os::unix::fs::symlink;

        let root_dir = tempdir().unwrap();
        symlink("/tmp", root_dir.path().join("deployment")).unwrap();
        let root = AuthorizedRoot::open(root_dir.path()).unwrap();
        let relative = BundleRelativePath::parse("deployment").unwrap();
        let authorized = root.authorize(&relative).unwrap();

        assert!(authorized.path().starts_with(root.canonical_path()));
        assert_eq!(authorized.path(), root.canonical_path().join("deployment"));
        assert!(
            fs::symlink_metadata(authorized.path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let observation = authorized.inspect().unwrap();
        assert_eq!(observation.kind, EntryKind::Symlink);
        assert_eq!(
            observation.raw_symlink_target.as_deref(),
            Some(Path::new("/tmp"))
        );
    }

    proptest! {
        #[test]
        fn accepted_components_never_escape_the_authorized_root(
            components in prop::collection::vec("[A-Za-z0-9_-]{1,12}", 1..8),
        ) {
            let root_dir = tempdir().unwrap();
            let root = AuthorizedRoot::open(root_dir.path()).unwrap();
            let relative = BundleRelativePath::parse(&components.join("/")).unwrap();
            let path = root.authorize(&relative).unwrap();
            prop_assert!(path.path().starts_with(root.canonical_path()));
            prop_assert!(!path.relative().as_str().split('/').any(|part| part == ".."));
        }

        #[test]
        fn arbitrary_untrusted_strings_cannot_escape_when_accepted(value in any::<String>()) {
            let root_dir = tempdir().unwrap();
            let root = AuthorizedRoot::open(root_dir.path()).unwrap();
            if let Ok(relative) = BundleRelativePath::parse(&value) {
                let path = root.authorize(&relative).unwrap();
                prop_assert!(path.path().starts_with(root.canonical_path()));
            }
        }
    }
}
