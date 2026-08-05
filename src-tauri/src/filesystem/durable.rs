use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use thiserror::Error;
use uuid::Uuid;

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
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        before_rename()?;
        fs::rename(&temporary, path)?;
        sync_directory(parent)?;
        Ok(())
    })();

    if result.is_err() {
        match fs::remove_file(&temporary) {
            Ok(()) => {
                let _ = sync_directory(parent);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {}
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
    fn corrupt_copy_retains_source_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("manifest.json");
        fs::write(&path, b"not-json").unwrap();

        let backup = preserve_corrupt_copy(&path).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"not-json");
        assert_eq!(fs::read(backup).unwrap(), b"not-json");
    }
}
