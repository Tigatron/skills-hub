use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use rustix::fs::{RenameFlags, renameat_with};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnedDirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OwnedFileIdentity {
    device: u64,
    inode: u64,
}

/// Captures no-follow identity for one application-created directory.
pub(crate) fn owned_directory_identity(path: &Path) -> io::Result<OwnedDirectoryIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("owned path is not a real directory"));
    }
    Ok(OwnedDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Captures regular-file identity from the same no-follow metadata used for ownership decisions.
pub(crate) fn owned_file_identity_from_metadata(
    metadata: &fs::Metadata,
) -> io::Result<OwnedFileIdentity> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::other("owned path is not a regular file"));
    }
    Ok(OwnedFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

impl OwnedFileIdentity {
    pub(crate) fn matches(self, metadata: &fs::Metadata) -> bool {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
    }
}

impl OwnedDirectoryIdentity {
    pub(crate) fn matches(self, metadata: &fs::Metadata) -> bool {
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
    }
}

/// Removes an application-created directory only after moving and revalidating its exact inode.
///
/// Returns `false` without deleting when the original path is absent or no longer has the captured
/// identity. A raced replacement moved during cleanup is restored when possible and otherwise left
/// in the quarantine path; neither mismatch is recursively removed.
pub(crate) fn remove_owned_directory(
    path: &Path,
    identity: OwnedDirectoryIdentity,
) -> io::Result<bool> {
    remove_owned_directory_with(path, identity, || Ok(()))
}

fn remove_owned_directory_with<F>(
    path: &Path,
    identity: OwnedDirectoryIdentity,
    before_rename: F,
) -> io::Result<bool>
where
    F: FnOnce() -> io::Result<()>,
{
    match fs::symlink_metadata(path) {
        Ok(metadata) if matches_directory_identity(&metadata, identity) => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    before_rename()?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("owned directory has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("owned directory has no name"))?;
    let quarantine = parent.join(format!(
        ".{}.cleanup-{}",
        name.to_string_lossy(),
        Uuid::now_v7()
    ));
    let parent_directory = File::open(parent)?;
    renameat_with(
        &parent_directory,
        name,
        &parent_directory,
        quarantine
            .file_name()
            .ok_or_else(|| io::Error::other("cleanup quarantine has no name"))?,
        RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)?;
    parent_directory.sync_all()?;

    let matches = fs::symlink_metadata(&quarantine)
        .is_ok_and(|metadata| matches_directory_identity(&metadata, identity));
    if !matches {
        if fs::symlink_metadata(path).is_err() {
            let _ = renameat_with(
                &parent_directory,
                quarantine
                    .file_name()
                    .ok_or_else(|| io::Error::other("cleanup quarantine has no name"))?,
                &parent_directory,
                name,
                RenameFlags::NOREPLACE,
            );
            let _ = parent_directory.sync_all();
        }
        return Ok(false);
    }
    fs::remove_dir_all(&quarantine)?;
    parent_directory.sync_all()?;
    Ok(true)
}

fn matches_directory_identity(metadata: &fs::Metadata, identity: OwnedDirectoryIdentity) -> bool {
    identity.matches(metadata)
}

/// Atomically replaces one file after flushing both its bytes and parent directory.
///
/// The destination parent must already exist. Temporary files are created beside the
/// destination so the final rename remains on one filesystem.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), DurableWriteError> {
    atomic_write_with(path, bytes, || Ok(()))
}

fn atomic_write_with<F>(
    path: &Path,
    bytes: &[u8],
    before_rename: F,
) -> Result<(), DurableWriteError>
where
    F: FnOnce() -> io::Result<()>,
{
    let parent = path.parent().ok_or(DurableWriteError::MissingParent)?;
    let temporary = temporary_sibling(path)?;
    let mut temporary_identity = None;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        let metadata = file.metadata()?;
        temporary_identity = Some((metadata.dev(), metadata.ino()));
        before_rename()?;
        fs::rename(&temporary, path)?;
        sync_directory(parent)?;
        Ok(())
    })();

    if result.is_err() {
        let owned = temporary_identity.is_some_and(|(device, inode)| {
            fs::symlink_metadata(&temporary).is_ok_and(|metadata| {
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.dev() == device
                    && metadata.ino() == inode
            })
        });
        match owned.then(|| fs::remove_file(&temporary)) {
            Some(Ok(())) => {
                let _ = sync_directory(parent);
            }
            Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Some(Err(_)) | None => {}
        }
    }
    result.map_err(DurableWriteError::Io)
}

/// Creates a durable diagnostic copy without replacing or renaming the source.
pub(crate) fn preserve_corrupt_copy(path: &Path) -> Result<PathBuf, DurableWriteError> {
    let parent = path.parent().ok_or(DurableWriteError::MissingParent)?;
    let file_name = path.file_name().ok_or(DurableWriteError::MissingFileName)?;
    let mut backup_name = OsString::from(file_name);
    backup_name.push(format!(".corrupt-{}", Uuid::now_v7()));
    let backup = parent.join(backup_name);

    let mut source = File::open(path)?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)?;
    io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    sync_directory(parent)?;
    Ok(backup)
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn temporary_sibling(path: &Path) -> Result<PathBuf, DurableWriteError> {
    let parent = path.parent().ok_or(DurableWriteError::MissingParent)?;
    let file_name = path.file_name().ok_or(DurableWriteError::MissingFileName)?;
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(".{}.tmp", Uuid::now_v7()));
    Ok(parent.join(temporary_name))
}

#[derive(Debug, Error)]
pub(crate) enum DurableWriteError {
    #[error("durable file path has no parent directory")]
    MissingParent,
    #[error("durable file path has no file name")]
    MissingFileName,
    #[error("durable file operation failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_failure_preserves_the_complete_old_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("manifest.json");
        fs::write(&path, b"old-complete").unwrap();

        let error = atomic_write_with(&path, b"new-complete", || {
            Err(io::Error::other("injected before rename"))
        })
        .unwrap_err();

        assert!(matches!(error, DurableWriteError::Io(_)));
        assert_eq!(fs::read(&path).unwrap(), b"old-complete");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn atomic_failure_never_deletes_a_replacement_at_the_temporary_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("manifest.json");
        fs::write(&path, b"old-complete").unwrap();
        let displaced = directory.path().join("displaced-staging");
        let mut replacement = None;

        let _ = atomic_write_with(&path, b"new-complete", || {
            let temporary = fs::read_dir(directory.path())?
                .map(Result::unwrap)
                .map(|entry| entry.path())
                .find(|candidate| candidate != &path)
                .expect("temporary sibling exists");
            fs::rename(&temporary, &displaced)?;
            fs::write(&temporary, b"unrelated replacement")?;
            replacement = Some(temporary);
            Err(io::Error::other("injected replacement"))
        });

        assert_eq!(fs::read(&path).unwrap(), b"old-complete");
        assert_eq!(fs::read(&displaced).unwrap(), b"new-complete");
        assert_eq!(
            fs::read(replacement.unwrap()).unwrap(),
            b"unrelated replacement"
        );
    }

    #[test]
    fn owned_directory_cleanup_restores_a_raced_replacement_without_deleting_it() {
        let directory = tempfile::tempdir().unwrap();
        let owned = directory.path().join("owned-staging");
        let displaced = directory.path().join("displaced-owned-staging");
        fs::create_dir(&owned).unwrap();
        fs::write(owned.join("owned"), b"owned").unwrap();
        let identity = owned_directory_identity(&owned).unwrap();

        let removed = remove_owned_directory_with(&owned, identity, || {
            fs::rename(&owned, &displaced)?;
            fs::create_dir(&owned)?;
            fs::write(owned.join("replacement"), b"preserve")
        })
        .unwrap();

        assert!(!removed);
        assert_eq!(fs::read(owned.join("replacement")).unwrap(), b"preserve");
        assert_eq!(fs::read(displaced.join("owned")).unwrap(), b"owned");
    }

    #[test]
    fn corrupt_copy_retains_source_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("manifest.json");
        fs::write(&path, b"not-json").unwrap();

        let backup = preserve_corrupt_copy(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"not-json");
        assert_eq!(fs::read(backup).unwrap(), b"not-json");
    }
}
