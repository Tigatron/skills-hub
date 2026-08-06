use std::{
    ffi::OsString,
    fs::{self, File},
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{domain::UtcTimestamp, filesystem::durable::sync_directory};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MIGRATION_TABLE: &str = "
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY NOT NULL CHECK (version > 0),
    name TEXT NOT NULL UNIQUE,
    checksum TEXT NOT NULL CHECK (length(checksum) = 64),
    applied_at_ms INTEGER NOT NULL
) STRICT;
";

#[derive(Debug, Clone, Copy)]
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
    checksum: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial",
        sql: include_str!("migrations/0001_initial.sql"),
        checksum: "6bbfca126d052df3a78e21e9345ac2cb33e2c50b8623a3a279fa316a3d926a9c",
    },
    Migration {
        version: 2,
        name: "scanner_projection",
        sql: include_str!("migrations/0002_scanner_projection.sql"),
        checksum: "c21f1379df37b97654776532f8023298009fb261ca7fdb148716a6fad47134a2",
    },
    Migration {
        version: 3,
        name: "activity_projection",
        sql: include_str!("migrations/0003_activity_projection.sql"),
        checksum: "1645cae6c411a15501f38b4b6e202dcfe78f8966ebf938d3dc98cb1de42c5887",
    },
    Migration {
        version: 4,
        name: "adapter_configurations",
        sql: include_str!("migrations/0004_adapter_configurations.sql"),
        checksum: "34f4e62050954fef9cdcb2a86d23fae5769c69169ef669c561bd35f80c4bc3dd",
    },
    Migration {
        version: 5,
        name: "workspace_authorization_identity",
        sql: include_str!("migrations/0005_workspace_authorization_identity.sql"),
        checksum: "7d35c45b3e797bf21e397cc415b593d4ef4acb67c285b1cfc232ec1e55834636",
    },
    Migration {
        version: 6,
        name: "library_perf_indexes",
        sql: include_str!("migrations/0006_library_perf_indexes.sql"),
        checksum: "b915c678dce5e514e0053a1a3fe4fb45dbab8e1153aa94816dc13d2cdcb2926a",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseSettings {
    pub foreign_keys: bool,
    pub journal_mode: String,
    pub synchronous: u8,
    pub busy_timeout_millis: u64,
    pub schema_version: u32,
}

pub(crate) fn open_database(path: &Path) -> Result<Connection, MigrationError> {
    let mut connection = Connection::open(path)?;
    configure_connection(&connection)?;
    apply_migrations(&mut connection, path, MIGRATIONS)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<(), MigrationError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    let mode: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(MigrationError::WalUnavailable { actual: mode });
    }
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn apply_migrations(
    connection: &mut Connection,
    database_path: &Path,
    migrations: &[Migration],
) -> Result<(), MigrationError> {
    verify_embedded_checksums(migrations)?;
    connection.execute_batch(MIGRATION_TABLE)?;
    validate_history(connection, migrations)?;

    let applied_version = current_version(connection)?;
    let pending: Vec<_> = migrations
        .iter()
        .copied()
        .filter(|migration| migration.version > applied_version)
        .collect();
    if pending.is_empty() {
        return Ok(());
    }

    if applied_version > 0 {
        create_pre_migration_backup(connection, database_path, applied_version)?;
    }

    connection.pragma_update(None, "synchronous", "FULL")?;
    let migration_result = (|| {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for migration in pending {
            transaction.execute_batch(migration.sql)?;
            transaction.execute(
                "INSERT INTO schema_migrations(version, name, checksum, applied_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    migration.version,
                    migration.name,
                    migration.checksum,
                    UtcTimestamp::now().unix_millis()?
                ],
            )?;
        }
        let final_version = migrations.last().map_or(0, |migration| migration.version);
        transaction.pragma_update(None, "user_version", final_version)?;
        transaction.commit()?;
        Ok::<(), MigrationError>(())
    })();
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    migration_result?;
    validate_history(connection, migrations)?;
    integrity_check(connection)?;

    Ok(())
}

fn verify_embedded_checksums(migrations: &[Migration]) -> Result<(), MigrationError> {
    let mut expected_version = 1;
    for migration in migrations {
        if migration.version != expected_version {
            return Err(MigrationError::NonContiguousDefinition {
                expected: expected_version,
                actual: migration.version,
            });
        }
        let actual = hex::encode(Sha256::digest(migration.sql.as_bytes()));
        if actual != migration.checksum {
            return Err(MigrationError::EmbeddedChecksumChanged {
                version: migration.version,
                expected: migration.checksum.to_owned(),
                actual,
            });
        }
        expected_version += 1;
    }
    Ok(())
}

fn validate_history(
    connection: &Connection,
    migrations: &[Migration],
) -> Result<(), MigrationError> {
    let mut statement = connection
        .prepare("SELECT version, name, checksum FROM schema_migrations ORDER BY version ASC")?;
    let applied = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (index, (version, name, checksum)) in applied.iter().enumerate() {
        let expected_version =
            u32::try_from(index + 1).map_err(|_| MigrationError::AppliedVersionOverflow)?;
        if *version != expected_version {
            return Err(MigrationError::NonContiguousHistory {
                expected: expected_version,
                actual: *version,
            });
        }
        let migration = migrations
            .iter()
            .find(|migration| migration.version == *version)
            .ok_or(MigrationError::UnsupportedFutureVersion { found: *version })?;
        if name != migration.name || checksum != migration.checksum {
            return Err(MigrationError::HistoricalChecksumMismatch { version: *version });
        }
    }
    let declared_version = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let latest_supported = migrations.last().map_or(0, |migration| migration.version);
    if declared_version > latest_supported {
        return Err(MigrationError::UnsupportedFutureVersion {
            found: declared_version,
        });
    }
    let applied_version = applied.last().map_or(0, |(version, _, _)| *version);
    if declared_version != applied_version {
        return Err(MigrationError::UserVersionMismatch {
            declared: declared_version,
            applied: applied_version,
        });
    }
    Ok(())
}

fn current_version(connection: &Connection) -> Result<u32, MigrationError> {
    connection
        .query_row("SELECT max(version) FROM schema_migrations", [], |row| {
            row.get::<_, Option<u32>>(0)
        })
        .map(|version| version.unwrap_or(0))
        .map_err(MigrationError::Sqlite)
}

fn create_pre_migration_backup(
    connection: &Connection,
    database_path: &Path,
    version: u32,
) -> Result<PathBuf, MigrationError> {
    let parent = database_path
        .parent()
        .ok_or(MigrationError::InvalidDatabasePath)?;
    let backup = parent.join(format!(
        "index.sqlite.pre-migration-v{version}-{}.bak",
        Uuid::now_v7()
    ));
    connection.backup("main", &backup, None)?;
    File::open(&backup)?.sync_all()?;
    sync_directory(parent)?;
    Ok(backup)
}

fn integrity_check(connection: &Connection) -> Result<(), MigrationError> {
    let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        return Err(MigrationError::IntegrityCheckFailed { result });
    }
    Ok(())
}

pub(crate) fn inspect_database_settings(
    connection: &Connection,
) -> Result<DatabaseSettings, MigrationError> {
    let foreign_keys =
        connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, u8>(0))?;
    let journal_mode =
        connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
    let synchronous = connection.query_row("PRAGMA synchronous", [], |row| row.get::<_, u8>(0))?;
    let busy_timeout_millis =
        connection.query_row("PRAGMA busy_timeout", [], |row| row.get::<_, u64>(0))?;
    Ok(DatabaseSettings {
        foreign_keys: foreign_keys == 1,
        journal_mode,
        synchronous,
        busy_timeout_millis,
        schema_version: current_version(connection)?,
    })
}

/// Atomically swaps a validated replacement database into place while retaining the old index.
///
/// The caller must close every connection first. All paths must be siblings so both renames
/// stay on one filesystem.
///
/// # Errors
///
/// Returns an error when the replacement is invalid, paths are not siblings, or either the
/// forward swap or compensating rollback fails.
pub fn replace_database_file(
    current: &Path,
    replacement: &Path,
    backup: &Path,
) -> Result<(), MigrationError> {
    let parent = current
        .parent()
        .ok_or(MigrationError::InvalidDatabasePath)?;
    if replacement.parent() != Some(parent) || backup.parent() != Some(parent) {
        return Err(MigrationError::ReplacementMustBeSibling);
    }
    if backup.exists() {
        return Err(MigrationError::BackupAlreadyExists);
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = database_sidecar(current, suffix);
        if sidecar.exists() {
            return Err(MigrationError::HotDatabaseSidecar { path: sidecar });
        }
    }

    let replacement_connection = Connection::open(replacement)?;
    integrity_check(&replacement_connection)?;
    drop(replacement_connection);

    fs::rename(current, backup)?;
    if let Err(source) = fs::rename(replacement, current) {
        return match fs::rename(backup, current) {
            Ok(()) => Err(MigrationError::Io(source)),
            Err(rollback) => Err(MigrationError::ReplaceRollbackFailed { source, rollback }),
        };
    }
    sync_directory(parent)?;
    Ok(())
}

fn database_sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut path = OsString::from(database.as_os_str());
    path.push(suffix);
    PathBuf::from(path)
}

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("SQLite migration failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("migration timestamp failed: {0}")]
    Time(#[from] crate::domain::TimeError),
    #[error("SQLite WAL mode is unavailable (actual mode: {actual})")]
    WalUnavailable { actual: String },
    #[error("migration definition is not contiguous: expected {expected}, found {actual}")]
    NonContiguousDefinition { expected: u32, actual: u32 },
    #[error("migration history is not contiguous: expected {expected}, found {actual}")]
    NonContiguousHistory { expected: u32, actual: u32 },
    #[error("migration {version} SQL changed; expected {expected}, found {actual}")]
    EmbeddedChecksumChanged {
        version: u32,
        expected: String,
        actual: String,
    },
    #[error("applied migration {version} no longer matches its released checksum")]
    HistoricalChecksumMismatch { version: u32 },
    #[error("database uses unsupported future migration {found}")]
    UnsupportedFutureVersion { found: u32 },
    #[error("database user_version {declared} does not match applied migration {applied}")]
    UserVersionMismatch { declared: u32, applied: u32 },
    #[error("too many applied migrations to represent")]
    AppliedVersionOverflow,
    #[error("database path has no parent")]
    InvalidDatabasePath,
    #[error("database integrity check failed: {result}")]
    IntegrityCheckFailed { result: String },
    #[error("replacement database and backup must be siblings of the active index")]
    ReplacementMustBeSibling,
    #[error("database backup destination already exists")]
    BackupAlreadyExists,
    #[error("database replacement requires recovery/checkpoint of sidecar first: {path:?}")]
    HotDatabaseSidecar { path: PathBuf },
    #[error("database replacement failed and rollback also failed: {source}; rollback: {rollback}")]
    ReplaceRollbackFailed {
        source: std::io::Error,
        rollback: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;

    #[test]
    fn empty_database_migrates_idempotently_with_required_pragmas() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let connection = open_database(&path).unwrap();
        let settings = inspect_database_settings(&connection).unwrap();

        assert!(settings.foreign_keys);
        assert_eq!(settings.journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(settings.synchronous, 1);
        assert_eq!(settings.busy_timeout_millis, 5_000);
        assert_eq!(settings.schema_version, 6);
        drop(connection);

        let reopened = open_database(&path).unwrap();
        assert_eq!(current_version(&reopened).unwrap(), 6);
        assert_eq!(
            reopened
                .query_row("SELECT count(*) FROM schema_migrations", [], |row| row
                    .get::<_, u32>(0))
                .unwrap(),
            6
        );
    }

    #[test]
    fn populated_v1_observations_upgrade_to_the_scanner_projection_without_data_loss() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let mut connection = Connection::open(&path).unwrap();
        configure_connection(&connection).unwrap();
        apply_migrations(&mut connection, &path, &MIGRATIONS[..1]).unwrap();
        connection
            .execute(
                "INSERT INTO scan_runs(
                    id, root_kind, root_id, scope, state, coverage_json,
                    started_at_ms, completed_at_ms
                 ) VALUES ('scan-1', 'target', 'root-1', 'global', 'completed',
                           '{\"complete\":true}', 1000, 2000)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO observations(
                    id, skill_id, adapter_id, scope, project_id, display_path,
                    normalized_path, canonical_path, deployment_name, digest, status,
                    last_successful_run_id, observed_at_ms
                 ) VALUES (
                    'observation-1', NULL, 'universal-agent-skills@1', 'global', NULL,
                    '/skills/example', '/skills/example', '/skills/example', 'example',
                    'sha256-bundle-v1:0000000000000000000000000000000000000000000000000000000000000000',
                    'verified', 'scan-1', 2000
                 )",
                [],
            )
            .unwrap();

        apply_migrations(&mut connection, &path, MIGRATIONS).unwrap();

        let migrated = connection
            .query_row(
                "SELECT display_path, source_root_kind, source_root_id, first_seen_at_ms,
                        observed_at_ms, error_code, stale_at_ms
                 FROM observations WHERE id = 'observation-1'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            migrated,
            (
                "/skills/example".to_owned(),
                "target".to_owned(),
                "root-1".to_owned(),
                2_000,
                2_000,
                None,
                None,
            )
        );
        assert_eq!(current_version(&connection).unwrap(), 6);
    }

    #[test]
    fn changed_historical_checksum_blocks_opening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let connection = open_database(&path).unwrap();
        connection
            .execute(
                "UPDATE schema_migrations SET checksum = ?1 WHERE version = 1",
                ["0".repeat(64)],
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            open_database(&path),
            Err(MigrationError::HistoricalChecksumMismatch { version: 1 })
        ));
    }

    #[test]
    fn unsupported_future_user_version_blocks_opening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let connection = open_database(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);

        assert!(matches!(
            open_database(&path),
            Err(MigrationError::UnsupportedFutureVersion { found: 99 })
        ));
    }

    #[test]
    fn failed_upgrade_rolls_back_and_retains_pre_upgrade_backup() {
        const THIRD_SQL: &str = "CREATE TABLE partial(value TEXT) STRICT; INVALID SQL;";
        let third_checksum = hex::encode(Sha256::digest(THIRD_SQL.as_bytes()));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.sqlite");
        let mut connection = open_database(&path).unwrap();
        let migrations = [
            MIGRATIONS[0],
            MIGRATIONS[1],
            MIGRATIONS[2],
            MIGRATIONS[3],
            MIGRATIONS[4],
            MIGRATIONS[5],
            Migration {
                version: 7,
                name: "failing",
                sql: THIRD_SQL,
                checksum: Box::leak(third_checksum.into_boxed_str()),
            },
        ];

        assert!(matches!(
            apply_migrations(&mut connection, &path, &migrations),
            Err(MigrationError::Sqlite(_))
        ));
        assert!(
            connection
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'partial'",
                    [],
                    |row| row.get::<_, u8>(0),
                )
                .optional()
                .unwrap()
                .is_none()
        );
        assert_eq!(current_version(&connection).unwrap(), 6);
        assert!(fs::read_dir(directory.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-migration-v6")
        }));
    }

    #[test]
    fn replacement_keeps_the_old_database_as_a_backup() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("index.sqlite");
        let replacement = directory.path().join("replacement.sqlite");
        let backup = directory.path().join("index.backup.sqlite");
        let old = Connection::open(&current).unwrap();
        old.execute_batch("CREATE TABLE marker(value TEXT); INSERT INTO marker VALUES ('old');")
            .unwrap();
        drop(old);
        let new = Connection::open(&replacement).unwrap();
        new.execute_batch("CREATE TABLE marker(value TEXT); INSERT INTO marker VALUES ('new');")
            .unwrap();
        drop(new);

        replace_database_file(&current, &replacement, &backup).unwrap();

        let current_value: String = Connection::open(&current)
            .unwrap()
            .query_row("SELECT value FROM marker", [], |row| row.get(0))
            .unwrap();
        let backup_value: String = Connection::open(&backup)
            .unwrap()
            .query_row("SELECT value FROM marker", [], |row| row.get(0))
            .unwrap();
        assert_eq!(current_value, "new");
        assert_eq!(backup_value, "old");
    }

    #[test]
    fn replacement_refuses_a_hot_wal_or_shm_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("index.sqlite");
        let replacement = directory.path().join("replacement.sqlite");
        let backup = directory.path().join("index.backup.sqlite");
        Connection::open(&current).unwrap().close().unwrap();
        Connection::open(&replacement).unwrap().close().unwrap();
        let wal = database_sidecar(&current, "-wal");
        fs::write(&wal, b"simulated hot WAL").unwrap();

        assert!(matches!(
            replace_database_file(&current, &replacement, &backup),
            Err(MigrationError::HotDatabaseSidecar { path }) if path == wal
        ));
        assert!(current.exists());
        assert!(replacement.exists());
        assert!(!backup.exists());
    }
}
