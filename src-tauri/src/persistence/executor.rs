use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use rusqlite::Connection;
use thiserror::Error;

use super::migrations::{
    DatabaseSettings, MigrationError, inspect_database_settings, open_database,
};

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

enum Message {
    Run(Job),
    Shutdown,
}

#[derive(Clone)]
pub struct DbExecutor {
    inner: Arc<DbExecutorInner>,
}

struct DbExecutorInner {
    sender: mpsc::Sender<Message>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl DbExecutor {
    /// Starts the dedicated database thread and opens/migrates its sole connection.
    ///
    /// # Errors
    ///
    /// Returns an error when the thread cannot start, `SQLite` cannot open, or migrations fail.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, DbExecutorError> {
        let path = path.into();
        let (sender, receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("skills-hub-db".to_owned())
            .spawn(move || database_thread(&path, &receiver, &ready_sender))?;

        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(DbExecutorInner {
                    sender,
                    thread: Mutex::new(Some(thread)),
                }),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(DbExecutorError::Migration(error))
            }
            Err(_) => {
                let _ = thread.join();
                Err(DbExecutorError::ThreadUnavailable)
            }
        }
    }

    /// Returns the connection settings observed on the dedicated thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the executor is unavailable or a `SQLite` pragma cannot be read.
    pub fn settings(&self) -> Result<DatabaseSettings, DbExecutorError> {
        self.execute(|connection| {
            inspect_database_settings(connection).map_err(DbExecutorError::Migration)
        })
    }

    /// Flushes and removes this connection's WAL before a caller-managed index replacement.
    pub(crate) fn checkpoint_for_replacement(&self) -> Result<(), DbExecutorError> {
        self.execute(|connection| {
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
    }

    pub(crate) fn execute<T, F>(&self, work: F) -> Result<T, DbExecutorError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DbExecutorError> + Send + 'static,
    {
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        self.inner
            .sender
            .send(Message::Run(Box::new(move |connection| {
                let result = work(connection);
                let _ = result_sender.send(result);
            })))
            .map_err(|_| DbExecutorError::ThreadUnavailable)?;
        result_receiver
            .recv()
            .map_err(|_| DbExecutorError::JobPanicked)?
    }

    pub(crate) fn execute_critical<T, F>(&self, work: F) -> Result<T, DbExecutorError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, DbExecutorError> + Send + 'static,
    {
        self.execute(move |connection| {
            connection.pragma_update(None, "synchronous", "FULL")?;
            let result = work(connection);
            let restore = connection.pragma_update(None, "synchronous", "NORMAL");
            match (result, restore) {
                (Ok(value), Ok(())) => Ok(value),
                (Err(error), _) => Err(error),
                (Ok(_), Err(error)) => Err(DbExecutorError::Sqlite(error)),
            }
        })
    }
}

impl DbExecutorInner {
    fn shutdown(&self) {
        let _ = self.sender.send(Message::Shutdown);
        if let Ok(mut thread) = self.thread.lock()
            && let Some(thread) = thread.take()
        {
            let _ = thread.join();
        }
    }
}

impl Drop for DbExecutorInner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn database_thread(
    path: &Path,
    receiver: &mpsc::Receiver<Message>,
    ready: &mpsc::SyncSender<Result<(), MigrationError>>,
) {
    let mut connection = match open_database(path) {
        Ok(connection) => {
            if ready.send(Ok(())).is_err() {
                return;
            }
            connection
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    while let Ok(message) = receiver.recv() {
        match message {
            Message::Run(job) => {
                let _ = catch_unwind(AssertUnwindSafe(|| job(&mut connection)));
            }
            Message::Shutdown => break,
        }
    }
    let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
}

#[derive(Debug, Error)]
pub enum DbExecutorError {
    #[error("database migration or configuration failed: {0}")]
    Migration(#[from] MigrationError),
    #[error("SQLite request failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database executor thread could not start: {0}")]
    ThreadStart(#[from] std::io::Error),
    #[error("database executor thread is unavailable")]
    ThreadUnavailable,
    #[error("database job panicked")]
    JobPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_requests_run_on_the_named_dedicated_thread() {
        let directory = tempfile::tempdir().unwrap();
        let executor = DbExecutor::open(directory.path().join("index.sqlite")).unwrap();

        let thread_name = executor
            .execute(|_| Ok(thread::current().name().unwrap_or("unnamed").to_owned()))
            .unwrap();

        assert_eq!(thread_name, "skills-hub-db");
        assert_eq!(executor.settings().unwrap().schema_version, 6);
    }

    #[test]
    fn a_panicking_job_does_not_kill_the_database_thread() {
        let directory = tempfile::tempdir().unwrap();
        let executor = DbExecutor::open(directory.path().join("index.sqlite")).unwrap();

        assert!(matches!(
            executor.execute::<(), _>(|_| panic!("injected database job panic")),
            Err(DbExecutorError::JobPanicked)
        ));
        assert_eq!(
            executor
                .execute(|connection| {
                    connection
                        .query_row("SELECT 42", [], |row| row.get::<_, u8>(0))
                        .map_err(DbExecutorError::Sqlite)
                })
                .unwrap(),
            42
        );
    }
}
