use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use rusqlite::{Connection, OptionalExtension, params, types::Type};
use serde_json::Value;
use thiserror::Error;

use crate::domain::{
    ActivityId, AdapterId, BundleDigest, BundleRelativePath, DeploymentHealth, DeploymentId,
    DeploymentMode, DeploymentName, ObservationId, OperationId, OperationOutcome, OperationState,
    ProjectId, ScanRunId, SkillId, SkillLifecycle, SnapshotId, TargetId, UtcTimestamp,
    WorkspaceRootId,
};

use super::executor::{DbExecutor, DbExecutorError};

#[derive(Clone)]
pub struct Repositories {
    database: DbExecutor,
}

impl Repositories {
    #[must_use]
    pub fn new(database: DbExecutor) -> Self {
        Self { database }
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_skill(&self, record: SkillRecord) -> Result<(), RepositoryError> {
        let working_path = record.working_path.to_string();
        let lifecycle = skill_lifecycle_text(record.lifecycle);
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO skills(
                    id, display_name, deployment_name, normalized_deployment_name,
                    working_path, working_digest, baseline_digest, lifecycle,
                    created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                    display_name = excluded.display_name,
                    deployment_name = excluded.deployment_name,
                    normalized_deployment_name = excluded.normalized_deployment_name,
                    working_path = excluded.working_path,
                    working_digest = excluded.working_digest,
                    baseline_digest = excluded.baseline_digest,
                    lifecycle = excluded.lifecycle,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    record.id.to_string(),
                    record.display_name,
                    record.deployment_name.as_str(),
                    record.deployment_name.collision_key(),
                    working_path,
                    record.working_digest.to_string(),
                    record.baseline_digest.to_string(),
                    lifecycle,
                    created_at,
                    updated_at,
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the database read or typed projection fails.
    pub fn skill(&self, id: SkillId) -> Result<Option<SkillRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT display_name, deployment_name, working_path, working_digest,
                                baseline_digest, lifecycle, created_at_ms, updated_at_ms
                         FROM skills WHERE id = ?1",
                        [id.to_string()],
                        |row| {
                            Ok(SkillRecord {
                                id,
                                display_name: row.get(0)?,
                                deployment_name: parse_deployment_name(
                                    &row.get::<_, String>(1)?,
                                    1,
                                )?,
                                working_path: parse_text(&row.get::<_, String>(2)?, 2)?,
                                working_digest: parse_text(&row.get::<_, String>(3)?, 3)?,
                                baseline_digest: parse_text(&row.get::<_, String>(4)?, 4)?,
                                lifecycle: parse_skill_lifecycle(&row.get::<_, String>(5)?, 5)?,
                                created_at: parse_millis(row.get(6)?, 6)?,
                                updated_at: parse_millis(row.get(7)?, 7)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns every indexed Skill in stable identifier order.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` is unavailable or an indexed value is invalid.
    pub fn skills(&self) -> Result<Vec<SkillRecord>, RepositoryError> {
        self.database
            .execute(|connection| {
                let mut statement = connection.prepare(
                    "SELECT id, display_name, deployment_name, working_path, working_digest,
                            baseline_digest, lifecycle, created_at_ms, updated_at_ms
                     FROM skills ORDER BY id",
                )?;
                statement
                    .query_map([], |row| {
                        let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                        Ok(SkillRecord {
                            id,
                            display_name: row.get(1)?,
                            deployment_name: parse_deployment_name(&row.get::<_, String>(2)?, 2)?,
                            working_path: parse_text(&row.get::<_, String>(3)?, 3)?,
                            working_digest: parse_text(&row.get::<_, String>(4)?, 4)?,
                            baseline_digest: parse_text(&row.get::<_, String>(5)?, 5)?,
                            lifecycle: parse_skill_lifecycle(&row.get::<_, String>(6)?, 6)?,
                            created_at: parse_millis(row.get(7)?, 7)?,
                            updated_at: parse_millis(row.get(8)?, 8)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Performs `SQLite`'s full integrity check. This is read-only.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot execute the integrity check.
    pub fn index_integrity(&self) -> Result<String, RepositoryError> {
        self.database
            .execute(|connection| {
                connection
                    .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns the number of rows reported by `SQLite`'s foreign-key checker.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot execute the foreign-key check.
    pub fn foreign_key_violation_count(&self) -> Result<u64, RepositoryError> {
        self.database
            .execute(|connection| {
                let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
                let mut rows = statement.query([])?;
                let mut count = 0_u64;
                while rows.next()?.is_some() {
                    count = count.saturating_add(1);
                }
                Ok(count)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns every source recorded for a Skill.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or a persisted value is invalid.
    pub fn skill_sources(&self, id: SkillId) -> Result<Vec<SkillSourceRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT kind, source_path, captured_at_ms, confidence FROM skill_sources
                 WHERE skill_id = ?1 ORDER BY id",
                )?;
                statement
                    .query_map([id.to_string()], |row| {
                        Ok(SkillSourceRecord {
                            skill_id: id,
                            kind: row.get(0)?,
                            path: PathBuf::from(row.get::<_, String>(1)?),
                            captured_at: parse_millis(row.get(2)?, 2)?,
                            confidence: row.get(3)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn insert_skill_source(&self, record: SkillSourceRecord) -> Result<(), RepositoryError> {
        let source_path = path_text(&record.path)?;
        let captured_at = millis(record.captured_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO skill_sources(skill_id, kind, source_path, captured_at_ms, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(skill_id, kind, source_path) DO UPDATE SET
                    captured_at_ms = excluded.captured_at_ms,
                    confidence = excluded.confidence",
                params![
                    record.skill_id.to_string(),
                    record.kind,
                    source_path,
                    captured_at,
                    record.confidence
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_object(&self, record: ObjectRecord) -> Result<(), RepositoryError> {
        let entry_count =
            i64::try_from(record.entry_count).map_err(|_| RepositoryError::IntegerOverflow)?;
        let byte_count =
            i64::try_from(record.byte_count).map_err(|_| RepositoryError::IntegerOverflow)?;
        let verified_at = millis(record.verified_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO objects(digest, relative_path, entry_count, byte_count, verified_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(digest) DO UPDATE SET
                    relative_path = excluded.relative_path,
                    entry_count = excluded.entry_count,
                    byte_count = excluded.byte_count,
                    verified_at_ms = excluded.verified_at_ms",
                params![record.digest.to_string(), record.relative_path.to_string(), entry_count,
                    byte_count, verified_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn insert_skill_revision(
        &self,
        record: SkillRevisionRecord,
    ) -> Result<(), RepositoryError> {
        let created_at = millis(record.created_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO skill_revisions(skill_id, digest, revision_kind, operation_id, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![record.skill_id.to_string(), record.digest.to_string(), record.kind,
                    record.operation_id.map(|id| id.to_string()), created_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_workspace_root(
        &self,
        record: WorkspaceRootRecord,
    ) -> Result<(), RepositoryError> {
        let selected_path = path_text(&record.selected_path)?;
        let canonical_path = path_text(&record.canonical_path)?;
        let ignores = json_text(&record.ignore_rules)?;
        let maximum_depth =
            i64::try_from(record.maximum_depth).map_err(|_| RepositoryError::IntegerOverflow)?;
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO workspace_roots(
                    id, selected_path, canonical_path, paused, maximum_depth,
                    ignore_rules_json, scan_status, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    selected_path = excluded.selected_path,
                    canonical_path = excluded.canonical_path,
                    paused = excluded.paused,
                    maximum_depth = excluded.maximum_depth,
                    ignore_rules_json = excluded.ignore_rules_json,
                    scan_status = excluded.scan_status,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    record.id.to_string(),
                    selected_path,
                    canonical_path,
                    record.paused,
                    maximum_depth,
                    ignores,
                    record.scan_status,
                    created_at,
                    updated_at
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Atomically persists a Workspace Root and the filesystem identity the user authorized.
    ///
    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the transaction fails.
    pub fn upsert_workspace_root_authorization(
        &self,
        record: WorkspaceRootRecord,
        identity: AuthorizationIdentityRecord,
    ) -> Result<(), RepositoryError> {
        let selected_path = path_text(&record.selected_path)?;
        let canonical_path = path_text(&record.canonical_path)?;
        let ignores = json_text(&record.ignore_rules)?;
        let maximum_depth =
            i64::try_from(record.maximum_depth).map_err(|_| RepositoryError::IntegerOverflow)?;
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO workspace_roots(
                    id, selected_path, canonical_path, paused, maximum_depth,
                    ignore_rules_json, scan_status, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    selected_path = excluded.selected_path,
                    canonical_path = excluded.canonical_path,
                    paused = excluded.paused,
                    maximum_depth = excluded.maximum_depth,
                    ignore_rules_json = excluded.ignore_rules_json,
                    scan_status = excluded.scan_status,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    record.id.to_string(),
                    selected_path,
                    canonical_path,
                    record.paused,
                    maximum_depth,
                    ignores,
                    record.scan_status,
                    created_at,
                    updated_at
                ],
            )?;
            transaction.execute(
                "INSERT INTO workspace_root_identities(workspace_root_id, device_id, file_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(workspace_root_id) DO UPDATE SET
                    device_id = excluded.device_id, file_id = excluded.file_id",
                params![
                    record.id.to_string(),
                    identity.device_id.to_string(),
                    identity.file_id.to_string()
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    /// Returns one Workspace Root by its stable authorization identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted data is invalid.
    pub fn workspace_root(
        &self,
        id: WorkspaceRootId,
    ) -> Result<Option<WorkspaceRootRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT selected_path, canonical_path, paused, maximum_depth,
                                ignore_rules_json, scan_status, created_at_ms, updated_at_ms
                         FROM workspace_roots WHERE id = ?1",
                        [id.to_string()],
                        |row| workspace_root_from_row(id, row),
                    )
                    .optional()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Persists the selected directory's stable filesystem identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the database write fails.
    pub fn set_workspace_root_identity(
        &self,
        id: WorkspaceRootId,
        identity: AuthorizationIdentityRecord,
    ) -> Result<(), RepositoryError> {
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO workspace_root_identities(workspace_root_id, device_id, file_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(workspace_root_id) DO UPDATE SET
                    device_id = excluded.device_id, file_id = excluded.file_id",
                params![
                    id.to_string(),
                    identity.device_id.to_string(),
                    identity.file_id.to_string()
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Returns the filesystem identity authorized for a Workspace Root.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read or integer projection fails.
    pub fn workspace_root_identity(
        &self,
        id: WorkspaceRootId,
    ) -> Result<Option<AuthorizationIdentityRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT device_id, file_id FROM workspace_root_identities
                 WHERE workspace_root_id = ?1",
                        [id.to_string()],
                        authorization_identity_from_row,
                    )
                    .optional()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns every authorized Workspace Root in stable path order.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted data is invalid.
    pub fn workspace_roots(&self) -> Result<Vec<WorkspaceRootRecord>, RepositoryError> {
        self.database
            .execute(|connection| {
                let mut statement = connection.prepare(
                    "SELECT id, selected_path, canonical_path, paused, maximum_depth,
                            ignore_rules_json, scan_status, created_at_ms, updated_at_ms
                     FROM workspace_roots ORDER BY canonical_path, id",
                )?;
                statement
                    .query_map([], |row| {
                        let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                        workspace_root_from_row_offset(id, row, 1)
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Removes one authorization boundary and its derived discovery/coverage projection.
    /// User content is never touched.
    ///
    /// # Errors
    ///
    /// Returns an error when the database transaction fails.
    pub fn remove_workspace_root(&self, id: WorkspaceRootId) -> Result<bool, RepositoryError> {
        self.database
            .execute(move |connection| {
                let transaction = connection.transaction()?;
                let id = id.to_string();
                transaction.execute(
                    "UPDATE observations
                     SET source_root_kind = 'manual_project', source_root_id = project_id
                     WHERE source_root_kind = 'workspace_root' AND source_root_id = ?1
                       AND project_id IN (SELECT id FROM projects WHERE manual = 1)",
                    [&id],
                )?;
                transaction.execute(
                    "DELETE FROM observations
                 WHERE source_root_kind = 'workspace_root' AND source_root_id = ?1",
                    [&id],
                )?;
                transaction.execute(
                    "DELETE FROM projects WHERE workspace_root_id = ?1 AND manual = 0",
                    [&id],
                )?;
                transaction.execute(
                    "DELETE FROM scan_runs WHERE root_kind = 'workspace_root' AND root_id = ?1",
                    [&id],
                )?;
                let removed =
                    transaction.execute("DELETE FROM workspace_roots WHERE id = ?1", [&id])?;
                transaction.commit()?;
                Ok(removed > 0)
            })
            .map_err(RepositoryError::Database)
    }

    /// Transfers positive evidence for manual projects out of a Workspace coverage boundary.
    /// This never marks evidence absent; a later complete scan owns that decision.
    ///
    /// # Errors
    ///
    /// Returns an error when the database update fails.
    pub fn rehome_workspace_manual_observations(
        &self,
        id: WorkspaceRootId,
    ) -> Result<(), RepositoryError> {
        self.database.execute(move |connection| {
            connection.execute(
                "UPDATE observations
                 SET source_root_kind = 'manual_project', source_root_id = project_id
                 WHERE source_root_kind = 'workspace_root' AND source_root_id = ?1
                   AND project_id IN (SELECT id FROM projects WHERE manual = 1)",
                [id.to_string()],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_project(&self, record: ProjectRecord) -> Result<(), RepositoryError> {
        let root_path = path_text(&record.root_path)?;
        let canonical_path = path_text(&record.canonical_path)?;
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO projects(
                    id, workspace_root_id, root_path, canonical_path, discovery_evidence,
                    git_classification, manual, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    workspace_root_id = excluded.workspace_root_id,
                    root_path = excluded.root_path,
                    canonical_path = excluded.canonical_path,
                    discovery_evidence = excluded.discovery_evidence,
                    git_classification = excluded.git_classification,
                    manual = excluded.manual,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    record.id.to_string(),
                    record.workspace_root_id.map(|id| id.to_string()),
                    root_path,
                    canonical_path,
                    record.discovery_evidence,
                    record.git_classification,
                    record.manual,
                    created_at,
                    updated_at
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Atomically persists a manually authorized project and its filesystem identity.
    ///
    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the transaction fails.
    pub fn upsert_manual_project_authorization(
        &self,
        record: ProjectRecord,
        identity: AuthorizationIdentityRecord,
    ) -> Result<(), RepositoryError> {
        let root_path = path_text(&record.root_path)?;
        let canonical_path = path_text(&record.canonical_path)?;
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "INSERT INTO projects(
                    id, workspace_root_id, root_path, canonical_path, discovery_evidence,
                    git_classification, manual, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    workspace_root_id = excluded.workspace_root_id,
                    root_path = excluded.root_path,
                    canonical_path = excluded.canonical_path,
                    discovery_evidence = excluded.discovery_evidence,
                    git_classification = excluded.git_classification,
                    manual = excluded.manual,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    record.id.to_string(),
                    record.workspace_root_id.map(|id| id.to_string()),
                    root_path,
                    canonical_path,
                    record.discovery_evidence,
                    record.git_classification,
                    record.manual,
                    created_at,
                    updated_at
                ],
            )?;
            transaction.execute(
                "INSERT INTO manual_project_identities(project_id, device_id, file_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id) DO UPDATE SET
                    device_id = excluded.device_id, file_id = excluded.file_id",
                params![
                    record.id.to_string(),
                    identity.device_id.to_string(),
                    identity.file_id.to_string()
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    /// Returns a project by its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted project data is invalid.
    pub fn project(&self, id: ProjectId) -> Result<Option<ProjectRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT workspace_root_id, root_path, canonical_path, discovery_evidence,
                    git_classification, manual, created_at_ms, updated_at_ms
             FROM projects WHERE id = ?1",
                        [id.to_string()],
                        |row| {
                            Ok(ProjectRecord {
                                id,
                                workspace_root_id: row
                                    .get::<_, Option<String>>(0)?
                                    .map(|v| parse_text(&v, 0))
                                    .transpose()?,
                                root_path: PathBuf::from(row.get::<_, String>(1)?),
                                canonical_path: PathBuf::from(row.get::<_, String>(2)?),
                                discovery_evidence: row.get(3)?,
                                git_classification: row.get(4)?,
                                manual: row.get(5)?,
                                created_at: parse_millis(row.get(6)?, 6)?,
                                updated_at: parse_millis(row.get(7)?, 7)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns projects discovered beneath one Workspace Root plus manually registered projects.
    ///
    /// # Errors
    ///
    /// Returns an error when persisted project data cannot be projected.
    pub fn workspace_projects(
        &self,
        workspace_root_id: WorkspaceRootId,
    ) -> Result<Vec<ProjectRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT id, workspace_root_id, root_path, canonical_path, discovery_evidence,
                        git_classification, manual, created_at_ms, updated_at_ms
                 FROM projects
                 WHERE workspace_root_id = ?1 OR manual = 1
                 ORDER BY canonical_path, id",
                )?;
                statement
                    .query_map([workspace_root_id.to_string()], |row| {
                        let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                        project_from_row_offset(id, row, 1)
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns a project with the same canonical filesystem identity, when indexed.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be encoded or the database read fails.
    pub fn project_by_canonical_path(
        &self,
        canonical_path: &Path,
    ) -> Result<Option<ProjectRecord>, RepositoryError> {
        let canonical_path = path_text(canonical_path)?;
        self.database
            .execute(move |connection| {
                connection.query_row(
                "SELECT id, workspace_root_id, root_path, canonical_path, discovery_evidence,
                        git_classification, manual, created_at_ms, updated_at_ms
                 FROM projects WHERE canonical_path = ?1",
                [canonical_path],
                |row| {
                    let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                    project_from_row_offset(id, row, 1)
                },
            ).optional().map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_target(&self, record: TargetRecord) -> Result<(), RepositoryError> {
        let root_path = path_text(&record.root_path)?;
        let canonical_root_path = path_text(&record.canonical_root_path)?;
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO targets(
                    id, adapter_id, scope, root_path, canonical_root_path, project_id,
                    is_override, is_custom, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET
                    adapter_id = excluded.adapter_id,
                    scope = excluded.scope,
                    root_path = excluded.root_path,
                    canonical_root_path = excluded.canonical_root_path,
                    project_id = excluded.project_id,
                    is_override = excluded.is_override,
                    is_custom = excluded.is_custom,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    record.id.to_string(),
                    record.adapter_id.to_string(),
                    record.scope,
                    root_path,
                    canonical_root_path,
                    record.project_id.map(|id| id.to_string()),
                    record.is_override,
                    record.is_custom,
                    created_at,
                    updated_at
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    /// Returns an error when persisted configuration cannot be read or projected.
    pub fn adapter_configurations(
        &self,
    ) -> Result<Vec<AdapterConfigurationRecord>, RepositoryError> {
        self.database.execute(|connection| {
            let mut statement = connection.prepare("SELECT adapter_name, adapter_id, enabled, global_override_path, project_override_path, created_at_ms, updated_at_ms FROM adapter_configurations ORDER BY adapter_name")?;
            statement.query_map([], |row| Ok(AdapterConfigurationRecord {
                adapter_name: row.get(0)?, adapter_id: parse_text(&row.get::<_, String>(1)?, 1)?, enabled: row.get(2)?,
                global_override_path: row.get::<_, Option<String>>(3)?.map(PathBuf::from), project_override_path: row.get(4)?,
                created_at: parse_millis(row.get(5)?, 5)?, updated_at: parse_millis(row.get(6)?, 6)?,
            }))?.collect::<Result<Vec<_>, _>>().map_err(DbExecutorError::Sqlite)
        }).map_err(RepositoryError::Database)
    }

    /// Persists the selected directory identity for a manual project.
    ///
    /// # Errors
    ///
    /// Returns an error when the database write fails.
    pub fn set_manual_project_identity(
        &self,
        id: ProjectId,
        identity: AuthorizationIdentityRecord,
    ) -> Result<(), RepositoryError> {
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO manual_project_identities(project_id, device_id, file_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id) DO UPDATE SET
                    device_id = excluded.device_id, file_id = excluded.file_id",
                params![
                    id.to_string(),
                    identity.device_id.to_string(),
                    identity.file_id.to_string()
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Returns the filesystem identity authorized for a manual project.
    ///
    /// # Errors
    ///
    /// Returns an error when the database read or integer projection fails.
    pub fn manual_project_identity(
        &self,
        id: ProjectId,
    ) -> Result<Option<AuthorizationIdentityRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection.query_row(
                "SELECT device_id, file_id FROM manual_project_identities WHERE project_id = ?1",
                [id.to_string()],
                authorization_identity_from_row,
            ).optional().map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns all manually authorized projects independently of Workspace discovery roots.
    ///
    /// # Errors
    ///
    /// Returns an error when persisted project data cannot be projected.
    pub fn manual_projects(&self) -> Result<Vec<ProjectRecord>, RepositoryError> {
        self.database
            .execute(|connection| {
                let mut statement = connection.prepare(
                    "SELECT id, workspace_root_id, root_path, canonical_path, discovery_evidence,
                        git_classification, manual, created_at_ms, updated_at_ms
                 FROM projects WHERE manual = 1 ORDER BY canonical_path, id",
                )?;
                statement
                    .query_map([], |row| {
                        let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                        project_from_row_offset(id, row, 1)
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// # Errors
    /// Returns an error when the configuration cannot be encoded or persisted.
    pub fn upsert_adapter_configuration(
        &self,
        record: AdapterConfigurationRecord,
    ) -> Result<(), RepositoryError> {
        let global = record
            .global_override_path
            .as_deref()
            .map(path_text)
            .transpose()?;
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            connection.execute("INSERT INTO adapter_configurations(adapter_name, adapter_id, enabled, global_override_path, project_override_path, created_at_ms, updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(adapter_name) DO UPDATE SET adapter_id=excluded.adapter_id, enabled=excluded.enabled, global_override_path=excluded.global_override_path, project_override_path=excluded.project_override_path, updated_at_ms=excluded.updated_at_ms", params![record.adapter_name, record.adapter_id.to_string(), record.enabled, global, record.project_override_path, created_at, updated_at])?;
            Ok(())
        }).map_err(RepositoryError::Database)
    }

    /// # Errors
    /// Returns an error when persisted registration metadata cannot be projected.
    pub fn target_registration_metadata(
        &self,
        id: TargetId,
    ) -> Result<Option<TargetRegistrationMetadataRecord>, RepositoryError> {
        self.database.execute(move |connection| connection.query_row("SELECT display_name, preferred_mode, root_device_id, root_file_id, override_kind, created_at_ms, updated_at_ms FROM target_registration_metadata WHERE target_id=?1", [id.to_string()], |row| Ok(TargetRegistrationMetadataRecord {
            target_id: id, display_name: row.get(0)?, preferred_mode: row.get::<_, Option<String>>(1)?.map(|v| parse_deployment_mode(&v, 1)).transpose()?, root_device_id: row.get::<_, String>(2)?.parse().map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(e)))?, root_file_id: row.get::<_, String>(3)?.parse().map_err(|e| rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(e)))?, override_kind: row.get(4)?, created_at: parse_millis(row.get(5)?, 5)?, updated_at: parse_millis(row.get(6)?, 6)?,
        })).optional().map_err(DbExecutorError::Sqlite)).map_err(RepositoryError::Database)
    }

    /// # Errors
    /// Returns an error when registration metadata cannot be persisted.
    pub fn upsert_target_registration_metadata(
        &self,
        record: TargetRegistrationMetadataRecord,
    ) -> Result<(), RepositoryError> {
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| { connection.execute("INSERT INTO target_registration_metadata(target_id, display_name, preferred_mode, root_device_id, root_file_id, override_kind, created_at_ms, updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(target_id) DO UPDATE SET display_name=excluded.display_name, preferred_mode=excluded.preferred_mode, root_device_id=excluded.root_device_id, root_file_id=excluded.root_file_id, override_kind=excluded.override_kind, updated_at_ms=excluded.updated_at_ms", params![record.target_id.to_string(), record.display_name, record.preferred_mode.map(deployment_mode_text), record.root_device_id.to_string(), record.root_file_id.to_string(), record.override_kind, created_at, updated_at])?; Ok(()) }).map_err(RepositoryError::Database)
    }

    /// Returns a target by its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted Target data is invalid.
    pub fn target(&self, id: TargetId) -> Result<Option<TargetRecord>, RepositoryError> {
        self.database.execute(move |connection| connection.query_row(
            "SELECT adapter_id, scope, root_path, canonical_root_path, project_id,
                    is_override, is_custom, created_at_ms, updated_at_ms FROM targets WHERE id = ?1",
            [id.to_string()], |row| target_from_row(id, row)
        ).optional().map_err(DbExecutorError::Sqlite)).map_err(RepositoryError::Database)
    }

    /// Returns targets in stable identifier order, bounded to 500 records.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted Target data is invalid.
    pub fn targets(&self, limit: usize) -> Result<Vec<TargetRecord>, RepositoryError> {
        let limit = i64::try_from(limit.min(500)).map_err(|_| RepositoryError::IntegerOverflow)?;
        self.database.execute(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, adapter_id, scope, root_path, canonical_root_path, project_id,
                        is_override, is_custom, created_at_ms, updated_at_ms FROM targets ORDER BY id LIMIT ?1")?;
            statement.query_map([limit], |row| {
                let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                target_from_row_offset(id, row, 1)
            })?.collect::<Result<Vec<_>, _>>().map_err(DbExecutorError::Sqlite)
        }).map_err(RepositoryError::Database)
    }

    /// Finds the target represented by its stable filesystem identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be encoded, the database is unavailable, or a
    /// persisted value is invalid.
    pub fn target_by_identity(
        &self,
        adapter_id: AdapterId,
        scope: String,
        project_id: Option<ProjectId>,
        canonical_root_path: &Path,
    ) -> Result<Option<TargetRecord>, RepositoryError> {
        let canonical_root_path = path_text(canonical_root_path)?;
        let project_id = project_id.map(|id| id.to_string());
        self.database.execute(move |connection| connection.query_row(
            "SELECT id, root_path, project_id, is_override, is_custom, created_at_ms, updated_at_ms
             FROM targets
             WHERE adapter_id = ?1
               AND scope = ?2
               AND canonical_root_path = ?3
               AND coalesce(project_id, '') = coalesce(?4, '')",
            params![adapter_id.to_string(), scope, canonical_root_path, project_id],
            |row| Ok(TargetRecord {
                id: parse_text(&row.get::<_, String>(0)?, 0)?,
                adapter_id: adapter_id.clone(), scope: scope.clone(),
                root_path: PathBuf::from(row.get::<_, String>(1)?),
                canonical_root_path: PathBuf::from(canonical_root_path.clone()),
                project_id: row.get::<_, Option<String>>(2)?.map(|v| parse_text(&v, 2)).transpose()?,
                is_override: row.get(3)?, is_custom: row.get(4)?,
                created_at: parse_millis(row.get(5)?, 5)?, updated_at: parse_millis(row.get(6)?, 6)?,
            }),
        ).optional().map_err(DbExecutorError::Sqlite)).map_err(RepositoryError::Database)
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_scan_run(&self, record: ScanRunRecord) -> Result<(), RepositoryError> {
        let coverage = json_text(&record.coverage)?;
        let started_at = millis(record.started_at)?;
        let completed_at = record.completed_at.map(millis).transpose()?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO scan_runs(
                    id, root_kind, root_id, scope, state, coverage_json,
                    started_at_ms, completed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                    state = excluded.state,
                    coverage_json = excluded.coverage_json,
                    completed_at_ms = excluded.completed_at_ms",
                params![
                    record.id.to_string(),
                    record.root_kind,
                    record.root_id,
                    record.scope,
                    record.state,
                    coverage,
                    started_at,
                    completed_at
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn append_scan_error(&self, record: ScanErrorRecord) -> Result<(), RepositoryError> {
        let path = path_text(&record.path)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO scan_errors(scan_run_id, path, error_code, summary)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    record.scan_run_id.to_string(),
                    path,
                    record.error_code,
                    record.summary
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Returns the latest durable coverage attempt and successful-complete timestamp for a
    /// Workspace Root, including inspectable diagnostics from that latest attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when persisted scan data cannot be projected.
    pub fn workspace_coverage(
        &self,
        id: WorkspaceRootId,
    ) -> Result<WorkspaceCoverageRecord, RepositoryError> {
        self.database
            .execute(move |connection| {
                let root_id = id.to_string();
                let latest = connection
                    .query_row(
                        "SELECT id, scope, state, coverage_json, started_at_ms, completed_at_ms
                 FROM scan_runs
                 WHERE root_kind = 'workspace_root' AND root_id = ?1
                 ORDER BY started_at_ms DESC, id DESC LIMIT 1",
                        [&root_id],
                        |row| workspace_scan_run_from_row(&root_id, row),
                    )
                    .optional()?;
                let last_successful_complete = connection
                    .query_row(
                        "SELECT completed_at_ms FROM scan_runs
                 WHERE root_kind = 'workspace_root' AND root_id = ?1
                   AND completed_at_ms IS NOT NULL
                   AND json_extract(coverage_json, '$.complete') = 1
                 ORDER BY completed_at_ms DESC LIMIT 1",
                        [&root_id],
                        |row| parse_millis(row.get(0)?, 0),
                    )
                    .optional()?;
                let mut errors = Vec::new();
                let mut total_errors = 0_u32;
                if let Some(run) = &latest {
                    total_errors = connection.query_row(
                        "SELECT count(*) FROM scan_errors WHERE scan_run_id = ?1",
                        [run.id.to_string()],
                        |row| row.get(0),
                    )?;
                    let mut statement = connection.prepare(
                        "SELECT path, error_code, summary FROM scan_errors
                     WHERE scan_run_id = ?1 ORDER BY id LIMIT 50",
                    )?;
                    errors = statement
                        .query_map([run.id.to_string()], |row| {
                            Ok(ScanErrorRecord {
                                scan_run_id: run.id,
                                path: PathBuf::from(row.get::<_, String>(0)?),
                                error_code: row.get(1)?,
                                summary: row.get(2)?,
                            })
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                }
                Ok(WorkspaceCoverageRecord {
                    latest,
                    last_successful_complete,
                    errors,
                    total_errors,
                })
            })
            .map_err(RepositoryError::Database)
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_observation(&self, record: ObservationRecord) -> Result<(), RepositoryError> {
        let values = ObservationValues::try_from(record)?;
        self.database.execute(move |connection| {
            upsert_observation(connection, &values)?;
            Ok(())
        })?;
        Ok(())
    }

    /// Returns one complete v2 observation projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or a persisted value is invalid.
    pub fn observation(
        &self,
        id: ObservationId,
    ) -> Result<Option<ObservationRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection.query_row(
            "SELECT skill_id, adapter_id, scope, project_id, source_root_kind, source_root_id,
                    display_path, normalized_path, canonical_path, deployment_name, digest, status,
                    error_code, error_summary, last_successful_run_id, first_seen_at_ms,
                    observed_at_ms, stale_at_ms FROM observations WHERE id = ?1",
            [id.to_string()], |row| observation_from_row(id, row)
        ).optional().map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns active observations with the same deployment name and, when supplied, digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or a persisted value is invalid.
    pub fn relevant_observations(
        &self,
        name: DeploymentName,
        digest: Option<BundleDigest>,
        limit: usize,
    ) -> Result<Vec<ObservationRecord>, RepositoryError> {
        let limit = i64::try_from(limit.min(500)).map_err(|_| RepositoryError::IntegerOverflow)?;
        self.database.execute(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, skill_id, adapter_id, scope, project_id, source_root_kind, source_root_id,
                        display_path, normalized_path, canonical_path, deployment_name, digest, status,
                        error_code, error_summary, last_successful_run_id, first_seen_at_ms,
                        observed_at_ms, stale_at_ms FROM observations
                 WHERE status <> 'stale'
                   AND (deployment_name = ?1 OR (?2 IS NOT NULL AND digest = ?2))
                 ORDER BY observed_at_ms DESC, id LIMIT ?3")?;
            statement.query_map(params![name.as_str(), digest.map(|v| v.to_string()), limit], |row| {
                let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                observation_from_row_offset(id, row, 1)
            })?.collect::<Result<Vec<_>, _>>().map_err(DbExecutorError::Sqlite)
        }).map_err(RepositoryError::Database)
    }

    /// Counts active Skill observations currently attributed to one Workspace Root.
    ///
    /// # Errors
    ///
    /// Returns an error when the database query fails or the count overflows.
    pub fn workspace_observation_count(&self, id: WorkspaceRootId) -> Result<u32, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT count(*) FROM observations
                 WHERE source_root_kind = 'workspace_root' AND source_root_id = ?1
                   AND status <> 'stale'",
                        [id.to_string()],
                        |row| row.get::<_, u32>(0),
                    )
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Counts projects owning at least one active Workspace observation.
    ///
    /// # Errors
    ///
    /// Returns an error when the database query fails.
    pub fn workspace_observed_project_count(
        &self,
        id: WorkspaceRootId,
    ) -> Result<u32, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT count(DISTINCT project_id) FROM observations
                         WHERE source_root_kind = 'workspace_root' AND source_root_id = ?1
                           AND project_id IS NOT NULL AND status <> 'stale'",
                        [id.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns all observations explicitly associated with a Skill.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or a persisted value is invalid.
    pub fn skill_observations(
        &self,
        id: SkillId,
    ) -> Result<Vec<ObservationRecord>, RepositoryError> {
        self.database.execute(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, skill_id, adapter_id, scope, project_id, source_root_kind, source_root_id,
                        display_path, normalized_path, canonical_path, deployment_name, digest, status,
                        error_code, error_summary, last_successful_run_id, first_seen_at_ms,
                        observed_at_ms, stale_at_ms FROM observations WHERE skill_id = ?1 ORDER BY id")?;
            statement.query_map([id.to_string()], |row| {
                let observation_id = parse_text(&row.get::<_, String>(0)?, 0)?;
                observation_from_row_offset(observation_id, row, 1)
            })?.collect::<Result<Vec<_>, _>>().map_err(DbExecutorError::Sqlite)
        }).map_err(RepositoryError::Database)
    }

    /// Atomically records a terminal scan, its diagnostics and observations, then applies
    /// absence/stale reconciliation only when this root's coverage completed successfully.
    ///
    /// # Errors
    ///
    /// Returns an error when records disagree about their coverage root, values cannot be
    /// projected, or the database transaction fails.
    pub fn reconcile_scan(&self, scan: ScanReconciliation) -> Result<u64, RepositoryError> {
        scan.validate()?;
        let mut activity = ActivityValues::try_from(scan.activity)?;
        activity.scan_run_id = Some(scan.run.id.to_string());
        let run = ScanRunValues::try_from(scan.run)?;
        let observations = scan
            .observations
            .into_iter()
            .map(ObservationValues::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let errors = scan
            .errors
            .into_iter()
            .map(ScanErrorValues::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let stale_at = run
            .completed_at
            .ok_or(RepositoryError::InvalidScanReconciliation(
                "terminal scan is missing completed_at",
            ))?;
        let run_id = run.id.clone();
        let adapter_id = scan.adapter_id.to_string();
        let scope = scan.scope;
        let source_root_kind = scan.source_root_kind;
        let source_root_id = scan.source_root_id;
        let coverage_complete = scan.coverage_complete;

        let stale_count = self.database.execute(move |connection| {
            let transaction = connection.transaction()?;
            upsert_scan_run(&transaction, &run)?;
            transaction.execute("DELETE FROM scan_errors WHERE scan_run_id = ?1", [&run_id])?;
            for error in errors {
                insert_scan_error(&transaction, &error)?;
            }
            for observation in &observations {
                upsert_observation(&transaction, observation)?;
            }
            insert_activity(&transaction, &activity)?;
            let stale_count = if coverage_complete {
                transaction.execute(
                    "UPDATE observations
                     SET status = 'stale', stale_at_ms = ?1
                     WHERE adapter_id = ?2
                       AND scope = ?3
                       AND source_root_kind = ?4
                       AND source_root_id = ?5
                       AND status <> 'stale'
                       AND (last_successful_run_id IS NULL OR last_successful_run_id <> ?6)",
                    params![
                        stale_at,
                        adapter_id,
                        scope,
                        source_root_kind,
                        source_root_id,
                        run_id
                    ],
                )?
            } else {
                0
            };
            transaction.commit()?;
            Ok(stale_count)
        })?;
        u64::try_from(stale_count).map_err(|_| RepositoryError::IntegerOverflow)
    }

    /// Returns active external observations used to build the M0 Library read model.
    ///
    /// # Errors
    ///
    /// Returns an error when the query or a typed projection fails.
    pub fn external_observations(&self) -> Result<Vec<ExternalObservationRecord>, RepositoryError> {
        self.database
            .execute(|connection| {
                let mut statement = connection.prepare(
                    "SELECT id, adapter_id, source_root_kind, source_root_id, display_path,
                            deployment_name, digest, status, error_code, error_summary,
                            first_seen_at_ms, observed_at_ms
                     FROM observations
                     WHERE skill_id IS NULL AND status <> 'stale'
                     ORDER BY deployment_name, normalized_path, id",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(ExternalObservationRecord {
                            id: parse_text(&row.get::<_, String>(0)?, 0)?,
                            adapter_id: parse_text(&row.get::<_, String>(1)?, 1)?,
                            source_root_kind: row.get(2)?,
                            source_root_id: row.get(3)?,
                            display_path: PathBuf::from(row.get::<_, String>(4)?),
                            deployment_name: parse_deployment_name(&row.get::<_, String>(5)?, 5)?,
                            digest: row
                                .get::<_, Option<String>>(6)?
                                .map(|value| parse_text(&value, 6))
                                .transpose()?,
                            status: row.get(7)?,
                            error_code: row.get(8)?,
                            error_summary: row.get(9)?,
                            first_seen_at: parse_millis(row.get(10)?, 10)?,
                            observed_at: parse_millis(row.get(11)?, 11)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns active managed symlink evidence for one adapter/scope.
    ///
    /// # Errors
    ///
    /// Returns an error when the query or a typed projection fails.
    pub fn managed_link_records(
        &self,
        adapter_id: AdapterId,
        scope: String,
    ) -> Result<Vec<ManagedLinkRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT deployment.skill_id, deployment.target_path,
                            deployment.expected_link_target
                     FROM deployments AS deployment
                     JOIN targets AS target ON target.id = deployment.target_id
                     WHERE deployment.active = 1
                       AND deployment.mode = 'symlink'
                       AND deployment.expected_link_target IS NOT NULL
                       AND target.adapter_id = ?1
                       AND target.scope = ?2
                     ORDER BY deployment.target_path",
                )?;
                statement
                    .query_map(params![adapter_id.to_string(), scope], |row| {
                        Ok(ManagedLinkRecord {
                            skill_id: parse_text(&row.get::<_, String>(0)?, 0)?,
                            target_path: PathBuf::from(row.get::<_, String>(1)?),
                            expected_target: PathBuf::from(row.get::<_, String>(2)?),
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_operation(&self, record: OperationRecord) -> Result<(), RepositoryError> {
        let values = OperationValues::try_from(record)?;
        self.database.execute(move |connection| {
            upsert_operation(connection, &values).map_err(DbExecutorError::Sqlite)
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_deployment(&self, record: DeploymentRecord) -> Result<(), RepositoryError> {
        let target_path = path_text(&record.target_path)?;
        let expected_link_target = record
            .expected_link_target
            .as_deref()
            .map(path_text)
            .transpose()?;
        let last_verified_at = record.last_verified_at.map(millis).transpose()?;
        let created_at = millis(record.created_at)?;
        let updated_at = millis(record.updated_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO deployments(
                    id, skill_id, target_id, deployment_name, normalized_deployment_name,
                    target_path, mode, expected_digest, expected_link_target, health,
                    adapter_version, active, last_verified_at_ms, last_operation_id,
                    created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(id) DO UPDATE SET
                    deployment_name = excluded.deployment_name,
                    normalized_deployment_name = excluded.normalized_deployment_name,
                    target_path = excluded.target_path,
                    mode = excluded.mode,
                    expected_digest = excluded.expected_digest,
                    expected_link_target = excluded.expected_link_target,
                    health = excluded.health,
                    adapter_version = excluded.adapter_version,
                    active = excluded.active,
                    last_verified_at_ms = excluded.last_verified_at_ms,
                    last_operation_id = excluded.last_operation_id,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    record.id.to_string(),
                    record.skill_id.to_string(),
                    record.target_id.to_string(),
                    record.deployment_name.as_str(),
                    record.deployment_name.collision_key(),
                    target_path,
                    deployment_mode_text(record.mode),
                    record.expected_digest.to_string(),
                    expected_link_target,
                    deployment_health_text(record.health),
                    record.adapter_version.to_string(),
                    record.active,
                    last_verified_at,
                    record.last_operation_id.map(|id| id.to_string()),
                    created_at,
                    updated_at
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Returns a deployment by its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted deployment data is invalid.
    pub fn deployment(
        &self,
        id: DeploymentId,
    ) -> Result<Option<DeploymentRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection.query_row(
            "SELECT skill_id, target_id, deployment_name, target_path, mode, expected_digest,
                    expected_link_target, health, adapter_version, active, last_verified_at_ms,
                    last_operation_id, created_at_ms, updated_at_ms FROM deployments WHERE id = ?1",
            [id.to_string()], |row| deployment_from_row_with_id(id, row)
        ).optional().map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns deployments matching all supplied filters in stable identifier order.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted deployment data is invalid.
    pub fn deployments(
        &self,
        skill_id: Option<SkillId>,
        target_id: Option<TargetId>,
        include_inactive: bool,
        limit: usize,
    ) -> Result<Vec<DeploymentRecord>, RepositoryError> {
        let limit = i64::try_from(limit.min(500)).map_err(|_| RepositoryError::IntegerOverflow)?;
        self.database.execute(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, skill_id, target_id, deployment_name, target_path, mode, expected_digest,
                        expected_link_target, health, adapter_version, active, last_verified_at_ms,
                        last_operation_id, created_at_ms, updated_at_ms FROM deployments
                 WHERE (?1 IS NULL OR skill_id = ?1) AND (?2 IS NULL OR target_id = ?2)
                   AND (?3 OR active = 1) ORDER BY id LIMIT ?4")?;
            statement.query_map(params![skill_id.map(|v| v.to_string()), target_id.map(|v| v.to_string()), include_inactive, limit], |row| {
                let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                deployment_from_row_with_id_offset(id, row, 1)
            })?.collect::<Result<Vec<_>, _>>().map_err(DbExecutorError::Sqlite)
        }).map_err(RepositoryError::Database)
    }

    /// Returns the active deployment colliding with a normalized target/name identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or persisted deployment data is invalid.
    pub fn active_deployment_for_target_name(
        &self,
        target_id: TargetId,
        name: DeploymentName,
    ) -> Result<Option<DeploymentRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection.query_row(
            "SELECT id, skill_id, target_id, deployment_name, target_path, mode, expected_digest,
                    expected_link_target, health, adapter_version, active, last_verified_at_ms,
                    last_operation_id, created_at_ms, updated_at_ms FROM deployments
             WHERE target_id = ?1 AND normalized_deployment_name = ?2 AND active = 1",
            params![target_id.to_string(), name.collision_key()], |row| {
                let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                deployment_from_row_with_id_offset(id, row, 1)
            }).optional().map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns every deployment without the interactive-list safety cap.
    ///
    /// Lifecycle integrity checks must never silently truncate their enumeration.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or a row is invalid.
    pub fn all_deployments(&self) -> Result<Vec<DeploymentRecord>, RepositoryError> {
        self.database.execute(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, skill_id, target_id, deployment_name, target_path, mode, expected_digest,
                        expected_link_target, health, adapter_version, active, last_verified_at_ms,
                        last_operation_id, created_at_ms, updated_at_ms FROM deployments ORDER BY id")?;
            statement.query_map([], |row| {
                let id = parse_text(&row.get::<_, String>(0)?, 0)?;
                deployment_from_row_with_id_offset(id, row, 1)
            })?.collect::<Result<Vec<_>, _>>().map_err(DbExecutorError::Sqlite)
        }).map_err(RepositoryError::Database)
    }

    /// Updates the persisted verification result for exactly one deployment.
    ///
    /// # Errors
    ///
    /// Returns an error when the deployment is absent or the database update fails.
    pub fn update_deployment_health(
        &self,
        id: DeploymentId,
        health: DeploymentHealth,
        verified_at: UtcTimestamp,
    ) -> Result<(), RepositoryError> {
        let verified_at = millis(verified_at)?;
        self.database.execute(move |connection| {
            let changed = connection.execute(
                "UPDATE deployments SET health = ?1, last_verified_at_ms = ?2 WHERE id = ?3",
                params![deployment_health_text(health), verified_at, id.to_string()],
            )?;
            if changed != 1 {
                return Err(DbExecutorError::Sqlite(
                    rusqlite::Error::QueryReturnedNoRows,
                ));
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Returns every deployment, active or inactive, belonging to a Skill.
    ///
    /// # Errors
    ///
    /// Returns an error when the database is unavailable or a persisted value is invalid.
    pub fn skill_deployments(&self, id: SkillId) -> Result<Vec<DeploymentRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT id, target_id, deployment_name, target_path, mode, expected_digest,
                        expected_link_target, health, adapter_version, active, last_verified_at_ms,
                        last_operation_id, created_at_ms, updated_at_ms
                 FROM deployments WHERE skill_id = ?1 ORDER BY id",
                )?;
                statement
                    .query_map([id.to_string()], |row| deployment_from_row(id, row))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_operation_step(
        &self,
        record: OperationStepRecord,
    ) -> Result<(), RepositoryError> {
        let ordinal =
            i64::try_from(record.ordinal).map_err(|_| RepositoryError::IntegerOverflow)?;
        let precondition = json_text(&record.precondition)?;
        let result = record.result.as_ref().map(json_text).transpose()?;
        let staging_path = record.staging_path.as_deref().map(path_text).transpose()?;
        let backup_path = record.backup_path.as_deref().map(path_text).transpose()?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO operation_steps(
                    operation_id, ordinal, action, precondition_json, staging_path,
                    backup_path, state, result_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(operation_id, ordinal) DO UPDATE SET
                    action = excluded.action,
                    precondition_json = excluded.precondition_json,
                    staging_path = excluded.staging_path,
                    backup_path = excluded.backup_path,
                    state = excluded.state,
                    result_json = excluded.result_json",
                params![
                    record.operation_id.to_string(),
                    ordinal,
                    record.action,
                    precondition,
                    staging_path,
                    backup_path,
                    record.state,
                    result
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_snapshot(&self, record: SnapshotRecord) -> Result<(), RepositoryError> {
        let created_at = millis(record.created_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO snapshots(id, operation_id, retention_state, protected, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    retention_state = excluded.retention_state,
                    protected = excluded.protected",
                params![
                    record.id.to_string(),
                    record.operation_id.to_string(),
                    record.retention_state,
                    record.protected,
                    created_at
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn upsert_snapshot_item(&self, record: SnapshotItemRecord) -> Result<(), RepositoryError> {
        let ordinal =
            i64::try_from(record.ordinal).map_err(|_| RepositoryError::IntegerOverflow)?;
        let fingerprint = record
            .entry_fingerprint
            .as_ref()
            .map(json_text)
            .transpose()?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO snapshot_items(
                    snapshot_id, ordinal, digest, entry_fingerprint_json, relation
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(snapshot_id, ordinal) DO UPDATE SET
                    digest = excluded.digest,
                    entry_fingerprint_json = excluded.entry_fingerprint_json,
                    relation = excluded.relation",
                params![
                    record.snapshot_id.to_string(),
                    ordinal,
                    record.digest.map(|digest| digest.to_string()),
                    fingerprint,
                    record.relation
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when values cannot be projected or the database write fails.
    pub fn append_activity(&self, record: ActivityRecord) -> Result<(), RepositoryError> {
        let values = ActivityValues::try_from(record)?;
        self.database.execute(move |connection| {
            insert_activity(connection, &values).map_err(DbExecutorError::Sqlite)
        })?;
        Ok(())
    }

    /// Idempotently projects Activity from durable evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when the record is invalid or the database write fails.
    pub fn upsert_activity(&self, record: ActivityRecord) -> Result<(), RepositoryError> {
        self.append_activity(record)
    }

    /// Returns a bounded, newest-first Activity page.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid bound, database failure, or invalid persisted value.
    pub fn activity_list(
        &self,
        query: ActivityQuery,
    ) -> Result<Vec<ActivityListRecord>, RepositoryError> {
        if query.limit == 0 || query.limit > 200 {
            return Err(RepositoryError::InvalidActivityLimit);
        }
        let limit = i64::from(query.limit);
        self.database
            .execute(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT id, kind, state, outcome, summary, started_at_ms, completed_at_ms,
                        operation_id, scan_run_id
                 FROM activity
                 WHERE (?1 IS NULL OR kind = ?1) AND (?2 IS NULL OR outcome = ?2)
                 ORDER BY started_at_ms DESC, id DESC LIMIT ?3",
                )?;
                statement
                    .query_map(
                        params![query.kind, query.outcome, limit],
                        activity_list_from_row,
                    )?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Returns one Activity detail projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the database fails or persisted detail is invalid.
    pub fn activity_detail(
        &self,
        id: ActivityId,
    ) -> Result<Option<ActivityDetailRecord>, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(
                        "SELECT id, kind, state, outcome, summary, started_at_ms, completed_at_ms,
                    operation_id, scan_run_id, details_json FROM activity WHERE id = ?1",
                        [id.to_string()],
                        activity_detail_from_row,
                    )
                    .optional()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    /// Finalizes the Operation and its user-facing Activity projection in one FULL transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when projection, the transaction, or synchronization mode fails.
    pub fn finalize_operation(
        &self,
        operation: OperationRecord,
        activity: ActivityRecord,
    ) -> Result<(), RepositoryError> {
        let operation = OperationValues::try_from(operation)?;
        let activity = ActivityValues::try_from(activity)?;
        self.database.execute_critical(move |connection| {
            let transaction = connection.transaction()?;
            upsert_operation(&transaction, &operation)?;
            insert_activity(&transaction, &activity)?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    /// Atomically publishes every durable read model produced by takeover.
    ///
    /// # Errors
    ///
    /// Returns an error when projection values disagree, cannot be encoded, or the critical
    /// `SQLite` transaction cannot commit durably.
    pub fn finalize_takeover(&self, projection: TakeoverProjection) -> Result<(), RepositoryError> {
        projection.validate()?;
        projection.validate_encodings()?;
        self.database.execute_critical(move |connection| {
            let transaction = connection.transaction()?;
            persist_takeover(&transaction, &projection)?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    /// Atomically publishes the durable projection of a deploy or undeploy operation.
    ///
    /// # Errors
    ///
    /// Returns an error when projection evidence disagrees or the critical transaction fails.
    pub fn finalize_deployment(
        &self,
        projection: DeploymentProjection,
    ) -> Result<(), RepositoryError> {
        projection.validate()?;
        projection.validate_encodings()?;
        self.database.execute_critical(move |connection| {
            let transaction = connection.transaction()?;
            persist_deployment(&transaction, &projection)?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    /// Atomically publishes all deployment read models and one idempotent Activity.
    ///
    /// # Errors
    ///
    /// Returns an error when projection evidence disagrees or the critical transaction fails.
    pub fn finalize_batch_deployment(
        &self,
        projection: BatchDeploymentProjection,
    ) -> Result<(), RepositoryError> {
        projection.validate()?;
        self.database.execute_critical(move |connection| {
            let transaction = connection.transaction()?;
            for deployment in &projection.deployments {
                persist_deployment(
                    &transaction,
                    &DeploymentProjection {
                        operation: projection.operation.clone(),
                        deployment: deployment.clone(),
                        snapshot: projection.snapshot.clone(),
                        snapshot_items: projection.snapshot_items.clone(),
                        activity: projection.activity.clone(),
                    },
                )?;
            }
            transaction.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the value cannot be serialized or the database write fails.
    pub fn set_setting(
        &self,
        key: String,
        value: &Value,
        updated_at: UtcTimestamp,
    ) -> Result<(), RepositoryError> {
        let value = json_text(value)?;
        let updated_at = millis(updated_at)?;
        self.database.execute(move |connection| {
            connection.execute(
                "INSERT INTO settings(key, value_json, updated_at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                    value_json = excluded.value_json,
                    updated_at_ms = excluded.updated_at_ms",
                params![key, value, updated_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    #[cfg(test)]
    fn table_count(&self, table: &'static str) -> Result<u32, RepositoryError> {
        self.database
            .execute(move |connection| {
                connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }

    #[cfg(test)]
    fn content_blob_columns(&self) -> Result<Vec<String>, RepositoryError> {
        self.database
            .execute(|connection| {
                let mut statement = connection.prepare(
                    "SELECT m.name || '.' || p.name
                     FROM sqlite_master AS m, pragma_table_info(m.name) AS p
                     WHERE m.type = 'table'
                       AND m.name NOT LIKE 'sqlite_%'
                       AND (upper(p.type) = 'BLOB' OR p.name IN ('content', 'bytes', 'bundle_blob'))",
                )?;
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbExecutorError::Sqlite)
            })
            .map_err(RepositoryError::Database)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRecord {
    pub id: SkillId,
    pub display_name: String,
    pub deployment_name: DeploymentName,
    pub working_path: BundleRelativePath,
    pub working_digest: BundleDigest,
    pub baseline_digest: BundleDigest,
    pub lifecycle: SkillLifecycle,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct SkillSourceRecord {
    pub skill_id: SkillId,
    pub kind: String,
    pub path: PathBuf,
    pub captured_at: UtcTimestamp,
    pub confidence: String,
}

#[derive(Debug, Clone)]
pub struct ObjectRecord {
    pub digest: BundleDigest,
    pub relative_path: BundleRelativePath,
    pub entry_count: u64,
    pub byte_count: u64,
    pub verified_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct SkillRevisionRecord {
    pub skill_id: SkillId,
    pub digest: BundleDigest,
    pub kind: String,
    pub operation_id: Option<OperationId>,
    pub created_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct WorkspaceRootRecord {
    pub id: WorkspaceRootId,
    pub selected_path: PathBuf,
    pub canonical_path: PathBuf,
    pub paused: bool,
    pub maximum_depth: usize,
    pub ignore_rules: Value,
    pub scan_status: String,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorizationIdentityRecord {
    pub device_id: u64,
    pub file_id: u64,
}

#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub id: ProjectId,
    pub workspace_root_id: Option<WorkspaceRootId>,
    pub root_path: PathBuf,
    pub canonical_path: PathBuf,
    pub discovery_evidence: String,
    pub git_classification: String,
    pub manual: bool,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct TargetRecord {
    pub id: TargetId,
    pub adapter_id: AdapterId,
    pub scope: String,
    pub root_path: PathBuf,
    pub canonical_root_path: PathBuf,
    pub project_id: Option<ProjectId>,
    pub is_override: bool,
    pub is_custom: bool,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct AdapterConfigurationRecord {
    pub adapter_name: String,
    pub adapter_id: AdapterId,
    pub enabled: bool,
    pub global_override_path: Option<PathBuf>,
    pub project_override_path: Option<String>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct TargetRegistrationMetadataRecord {
    pub target_id: TargetId,
    pub display_name: String,
    pub preferred_mode: Option<DeploymentMode>,
    pub root_device_id: u64,
    pub root_file_id: u64,
    pub override_kind: Option<String>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct ScanRunRecord {
    pub id: ScanRunId,
    pub root_kind: String,
    pub root_id: Option<String>,
    pub scope: String,
    pub state: String,
    pub coverage: Value,
    pub started_at: UtcTimestamp,
    pub completed_at: Option<UtcTimestamp>,
}

#[derive(Debug, Clone)]
pub struct ScanErrorRecord {
    pub scan_run_id: ScanRunId,
    pub path: PathBuf,
    pub error_code: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceCoverageRecord {
    pub latest: Option<ScanRunRecord>,
    pub last_successful_complete: Option<UtcTimestamp>,
    pub errors: Vec<ScanErrorRecord>,
    pub total_errors: u32,
}

#[derive(Debug, Clone)]
pub struct ObservationRecord {
    pub id: ObservationId,
    pub skill_id: Option<SkillId>,
    pub adapter_id: AdapterId,
    pub scope: String,
    pub project_id: Option<ProjectId>,
    pub source_root_kind: String,
    pub source_root_id: String,
    pub display_path: PathBuf,
    pub normalized_path: String,
    pub canonical_path: Option<PathBuf>,
    pub deployment_name: DeploymentName,
    pub digest: Option<BundleDigest>,
    pub status: String,
    pub error_code: Option<String>,
    pub error_summary: Option<String>,
    pub last_successful_run_id: Option<ScanRunId>,
    pub first_seen_at: UtcTimestamp,
    pub observed_at: UtcTimestamp,
    pub stale_at: Option<UtcTimestamp>,
}

#[derive(Debug, Clone)]
pub struct ScanReconciliation {
    pub run: ScanRunRecord,
    pub adapter_id: AdapterId,
    pub scope: String,
    pub source_root_kind: String,
    pub source_root_id: String,
    pub observations: Vec<ObservationRecord>,
    pub errors: Vec<ScanErrorRecord>,
    pub coverage_complete: bool,
    pub activity: ActivityRecord,
}

impl ScanReconciliation {
    fn validate(&self) -> Result<(), RepositoryError> {
        if self.run.state == "queued" || self.run.state == "running" {
            return Err(RepositoryError::InvalidScanReconciliation(
                "scan state is not terminal",
            ));
        }
        if self.run.root_kind != self.source_root_kind
            || self.run.root_id.as_deref() != Some(self.source_root_id.as_str())
            || self.run.scope != self.scope
        {
            return Err(RepositoryError::InvalidScanReconciliation(
                "scan run belongs to a different coverage root",
            ));
        }
        if self.observations.iter().any(|observation| {
            observation.adapter_id != self.adapter_id
                || observation.scope != self.scope
                || observation.source_root_kind != self.source_root_kind
                || observation.source_root_id != self.source_root_id
        }) {
            return Err(RepositoryError::InvalidScanReconciliation(
                "observation belongs to a different coverage root",
            ));
        }
        if self
            .errors
            .iter()
            .any(|error| error.scan_run_id != self.run.id)
        {
            return Err(RepositoryError::InvalidScanReconciliation(
                "diagnostic belongs to a different scan run",
            ));
        }
        if self.activity.operation_id.is_some() {
            return Err(RepositoryError::InvalidScanReconciliation(
                "activity belongs to a different scan run",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ExternalObservationRecord {
    pub id: ObservationId,
    pub adapter_id: AdapterId,
    pub source_root_kind: String,
    pub source_root_id: String,
    pub display_path: PathBuf,
    pub deployment_name: DeploymentName,
    pub digest: Option<BundleDigest>,
    pub status: String,
    pub error_code: Option<String>,
    pub error_summary: Option<String>,
    pub first_seen_at: UtcTimestamp,
    pub observed_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct ManagedLinkRecord {
    pub skill_id: SkillId,
    pub target_path: PathBuf,
    pub expected_target: PathBuf,
}

#[derive(Debug, Clone)]
pub struct OperationRecord {
    pub id: OperationId,
    pub plan_digest: String,
    pub operation_type: String,
    pub state: OperationState,
    pub outcome: Option<OperationOutcome>,
    pub recovery_state: Option<String>,
    pub journal_path: BundleRelativePath,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub finalized_at: Option<UtcTimestamp>,
}

#[derive(Debug, Clone)]
pub struct DeploymentRecord {
    pub id: DeploymentId,
    pub skill_id: SkillId,
    pub target_id: TargetId,
    pub deployment_name: DeploymentName,
    pub target_path: PathBuf,
    pub mode: DeploymentMode,
    pub expected_digest: BundleDigest,
    pub expected_link_target: Option<PathBuf>,
    pub health: DeploymentHealth,
    pub adapter_version: AdapterId,
    pub active: bool,
    pub last_verified_at: Option<UtcTimestamp>,
    pub last_operation_id: Option<OperationId>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct OperationStepRecord {
    pub operation_id: OperationId,
    pub ordinal: usize,
    pub action: String,
    pub precondition: Value,
    pub staging_path: Option<PathBuf>,
    pub backup_path: Option<PathBuf>,
    pub state: String,
    pub result: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    pub id: SnapshotId,
    pub operation_id: OperationId,
    pub retention_state: String,
    pub protected: bool,
    pub created_at: UtcTimestamp,
}

#[derive(Debug, Clone)]
pub struct SnapshotItemRecord {
    pub snapshot_id: SnapshotId,
    pub ordinal: usize,
    pub digest: Option<BundleDigest>,
    pub entry_fingerprint: Option<Value>,
    pub relation: String,
}

#[derive(Debug, Clone)]
pub struct ActivityRecord {
    pub id: ActivityId,
    pub operation_id: Option<OperationId>,
    pub kind: String,
    pub state: String,
    pub outcome: Option<OperationOutcome>,
    pub summary: String,
    pub details: Value,
    pub started_at: UtcTimestamp,
    pub completed_at: Option<UtcTimestamp>,
}

#[derive(Debug, Clone)]
pub struct ActivityQuery {
    pub kind: Option<String>,
    pub outcome: Option<String>,
    pub limit: u16,
}

#[derive(Debug, Clone)]
pub struct ActivityListRecord {
    pub id: ActivityId,
    pub kind: String,
    pub state: String,
    pub outcome: Option<String>,
    pub summary: String,
    pub started_at: UtcTimestamp,
    pub completed_at: Option<UtcTimestamp>,
    pub operation_id: Option<OperationId>,
    pub scan_run_id: Option<ScanRunId>,
}

#[derive(Debug, Clone)]
pub struct ActivityDetailRecord {
    pub item: ActivityListRecord,
    pub details: Value,
}

/// Complete persistence projection emitted after deploy or undeploy verification succeeds.
#[derive(Debug, Clone)]
pub struct DeploymentProjection {
    pub operation: OperationRecord,
    pub deployment: DeploymentRecord,
    pub snapshot: Option<SnapshotRecord>,
    pub snapshot_items: Vec<SnapshotItemRecord>,
    pub activity: ActivityRecord,
}

#[derive(Debug, Clone)]
pub struct BatchDeploymentProjection {
    pub operation: OperationRecord,
    pub deployments: Vec<DeploymentRecord>,
    pub snapshot: Option<SnapshotRecord>,
    pub snapshot_items: Vec<SnapshotItemRecord>,
    pub activity: ActivityRecord,
}

impl BatchDeploymentProjection {
    fn validate(&self) -> Result<(), RepositoryError> {
        if !(2..=20).contains(&self.deployments.len())
            || self.activity.operation_id != Some(self.operation.id)
            || self
                .deployments
                .iter()
                .any(|deployment| deployment.last_operation_id != Some(self.operation.id))
        {
            return Err(RepositoryError::InvalidDeploymentProjection(
                "batch deployment evidence belongs to a different operation",
            ));
        }
        for deployment in &self.deployments {
            let projection = DeploymentProjection {
                operation: self.operation.clone(),
                deployment: deployment.clone(),
                snapshot: self.snapshot.clone(),
                snapshot_items: self.snapshot_items.clone(),
                activity: self.activity.clone(),
            };
            projection.validate()?;
            projection.validate_encodings()?;
        }
        Ok(())
    }
}

impl DeploymentProjection {
    fn validate(&self) -> Result<(), RepositoryError> {
        let operation_id = self.operation.id;
        if self.deployment.last_operation_id != Some(operation_id)
            || self.activity.operation_id != Some(operation_id)
        {
            return Err(RepositoryError::InvalidDeploymentProjection(
                "deployment or activity belongs to a different operation",
            ));
        }
        match &self.snapshot {
            None if !self.snapshot_items.is_empty() => Err(
                RepositoryError::InvalidDeploymentProjection("snapshot items require a snapshot"),
            ),
            Some(snapshot)
                if snapshot.operation_id != operation_id
                    || self
                        .snapshot_items
                        .iter()
                        .any(|item| item.snapshot_id != snapshot.id) =>
            {
                Err(RepositoryError::InvalidDeploymentProjection(
                    "snapshot records belong to a different operation or snapshot",
                ))
            }
            _ => Ok(()),
        }
    }

    fn validate_encodings(&self) -> Result<(), RepositoryError> {
        OperationValues::try_from(self.operation.clone())?;
        path_text(&self.deployment.target_path)?;
        if let Some(path) = &self.deployment.expected_link_target {
            path_text(path)?;
        }
        millis(self.deployment.created_at)?;
        millis(self.deployment.updated_at)?;
        self.deployment.last_verified_at.map(millis).transpose()?;
        if let Some(snapshot) = &self.snapshot {
            millis(snapshot.created_at)?;
        }
        for item in &self.snapshot_items {
            i64::try_from(item.ordinal).map_err(|_| RepositoryError::IntegerOverflow)?;
            if let Some(fingerprint) = &item.entry_fingerprint {
                json_text(fingerprint)?;
            }
        }
        ActivityValues::try_from(self.activity.clone())?;
        Ok(())
    }
}

/// Complete persistence projection emitted after takeover filesystem verification succeeds.
#[derive(Debug, Clone)]
pub struct TakeoverProjection {
    pub operation: OperationRecord,
    pub skill: SkillRecord,
    pub sources: Vec<SkillSourceRecord>,
    pub object: ObjectRecord,
    pub revision: SkillRevisionRecord,
    pub targets: Vec<TargetRecord>,
    pub deployments: Vec<DeploymentRecord>,
    pub snapshot: Option<SnapshotRecord>,
    pub snapshot_items: Vec<SnapshotItemRecord>,
    pub observation_ids: Vec<ObservationId>,
    pub activity: ActivityRecord,
}

impl TakeoverProjection {
    fn validate(&self) -> Result<(), RepositoryError> {
        let skill = self.skill.id;
        let operation = self.operation.id;
        if self.sources.iter().any(|v| v.skill_id != skill)
            || self.revision.skill_id != skill
            || self.deployments.iter().any(|v| v.skill_id != skill)
            || self.revision.operation_id != Some(operation)
            || self.activity.operation_id != Some(operation)
            || self
                .deployments
                .iter()
                .any(|v| v.last_operation_id != Some(operation))
        {
            return Err(RepositoryError::InvalidTakeoverProjection(
                "records belong to different Skills or Operations",
            ));
        }
        if self.revision.digest != self.object.digest
            || self.skill.working_digest != self.object.digest
        {
            return Err(RepositoryError::InvalidTakeoverProjection(
                "object and revision digests disagree",
            ));
        }
        if self.deployments.iter().any(|deployment| {
            !self
                .targets
                .iter()
                .any(|target| target.id == deployment.target_id)
        }) {
            return Err(RepositoryError::InvalidTakeoverProjection(
                "deployment target is absent from projection",
            ));
        }
        match &self.snapshot {
            None if !self.snapshot_items.is_empty() => Err(
                RepositoryError::InvalidTakeoverProjection("snapshot items require a snapshot"),
            ),
            Some(snapshot)
                if snapshot.operation_id != operation
                    || self
                        .snapshot_items
                        .iter()
                        .any(|item| item.snapshot_id != snapshot.id) =>
            {
                Err(RepositoryError::InvalidTakeoverProjection(
                    "snapshot records belong to a different Operation or Snapshot",
                ))
            }
            _ => Ok(()),
        }
    }

    fn validate_encodings(&self) -> Result<(), RepositoryError> {
        for source in &self.sources {
            path_text(&source.path)?;
            millis(source.captured_at)?;
        }
        for target in &self.targets {
            path_text(&target.root_path)?;
            path_text(&target.canonical_root_path)?;
        }
        for deployment in &self.deployments {
            path_text(&deployment.target_path)?;
            if let Some(path) = &deployment.expected_link_target {
                path_text(path)?;
            }
        }
        for item in &self.snapshot_items {
            if let Some(value) = &item.entry_fingerprint {
                json_text(value)?;
            }
            i64::try_from(item.ordinal).map_err(|_| RepositoryError::IntegerOverflow)?;
        }
        ActivityValues::try_from(self.activity.clone())?;
        OperationValues::try_from(self.operation.clone())?;
        Ok(())
    }
}

struct ScanRunValues {
    id: String,
    root_kind: String,
    root_id: Option<String>,
    scope: String,
    state: String,
    coverage: String,
    started_at: i64,
    completed_at: Option<i64>,
}

impl TryFrom<ScanRunRecord> for ScanRunValues {
    type Error = RepositoryError;

    fn try_from(record: ScanRunRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id.to_string(),
            root_kind: record.root_kind,
            root_id: record.root_id,
            scope: record.scope,
            state: record.state,
            coverage: json_text(&record.coverage)?,
            started_at: millis(record.started_at)?,
            completed_at: record.completed_at.map(millis).transpose()?,
        })
    }
}

struct ScanErrorValues {
    scan_run_id: String,
    path: String,
    error_code: String,
    summary: String,
}

impl TryFrom<ScanErrorRecord> for ScanErrorValues {
    type Error = RepositoryError;

    fn try_from(record: ScanErrorRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            scan_run_id: record.scan_run_id.to_string(),
            path: path_text(&record.path)?,
            error_code: record.error_code,
            summary: record.summary,
        })
    }
}

struct ObservationValues {
    id: String,
    skill_id: Option<String>,
    adapter_id: String,
    scope: String,
    project_id: Option<String>,
    source_root_kind: String,
    source_root_id: String,
    display_path: String,
    normalized_path: String,
    canonical_path: Option<String>,
    deployment_name: String,
    digest: Option<String>,
    status: String,
    error_code: Option<String>,
    error_summary: Option<String>,
    last_successful_run_id: Option<String>,
    first_seen_at: i64,
    observed_at: i64,
    stale_at: Option<i64>,
}

impl TryFrom<ObservationRecord> for ObservationValues {
    type Error = RepositoryError;

    fn try_from(record: ObservationRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id.to_string(),
            skill_id: record.skill_id.map(|id| id.to_string()),
            adapter_id: record.adapter_id.to_string(),
            scope: record.scope,
            project_id: record.project_id.map(|id| id.to_string()),
            source_root_kind: record.source_root_kind,
            source_root_id: record.source_root_id,
            display_path: path_text(&record.display_path)?,
            normalized_path: record.normalized_path,
            canonical_path: record
                .canonical_path
                .as_deref()
                .map(path_text)
                .transpose()?,
            deployment_name: record.deployment_name.to_string(),
            digest: record.digest.map(|digest| digest.to_string()),
            status: record.status,
            error_code: record.error_code,
            error_summary: record.error_summary,
            last_successful_run_id: record.last_successful_run_id.map(|id| id.to_string()),
            first_seen_at: millis(record.first_seen_at)?,
            observed_at: millis(record.observed_at)?,
            stale_at: record.stale_at.map(millis).transpose()?,
        })
    }
}

struct OperationValues {
    id: String,
    plan_digest: String,
    operation_type: String,
    state: &'static str,
    outcome: Option<&'static str>,
    recovery_state: Option<String>,
    journal_path: String,
    created_at: i64,
    updated_at: i64,
    finalized_at: Option<i64>,
}

impl TryFrom<OperationRecord> for OperationValues {
    type Error = RepositoryError;

    fn try_from(record: OperationRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id.to_string(),
            plan_digest: record.plan_digest,
            operation_type: record.operation_type,
            state: operation_state_text(record.state),
            outcome: record.outcome.map(operation_outcome_text),
            recovery_state: record.recovery_state,
            journal_path: record.journal_path.to_string(),
            created_at: millis(record.created_at)?,
            updated_at: millis(record.updated_at)?,
            finalized_at: record.finalized_at.map(millis).transpose()?,
        })
    }
}

struct ActivityValues {
    id: String,
    operation_id: Option<String>,
    scan_run_id: Option<String>,
    kind: String,
    state: String,
    outcome: Option<&'static str>,
    summary: String,
    details: String,
    started_at: i64,
    completed_at: Option<i64>,
}

impl TryFrom<ActivityRecord> for ActivityValues {
    type Error = RepositoryError;

    fn try_from(record: ActivityRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id.to_string(),
            operation_id: record.operation_id.map(|id| id.to_string()),
            scan_run_id: None,
            kind: record.kind,
            state: record.state,
            outcome: record.outcome.map(operation_outcome_text),
            summary: record.summary,
            details: json_text(&record.details)?,
            started_at: millis(record.started_at)?,
            completed_at: record.completed_at.map(millis).transpose()?,
        })
    }
}

fn upsert_scan_run(connection: &Connection, value: &ScanRunValues) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO scan_runs(
            id, root_kind, root_id, scope, state, coverage_json,
            started_at_ms, completed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            state = excluded.state,
            coverage_json = excluded.coverage_json,
            completed_at_ms = excluded.completed_at_ms",
        params![
            value.id,
            value.root_kind,
            value.root_id,
            value.scope,
            value.state,
            value.coverage,
            value.started_at,
            value.completed_at
        ],
    )?;
    Ok(())
}

fn insert_scan_error(connection: &Connection, value: &ScanErrorValues) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO scan_errors(scan_run_id, path, error_code, summary)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            value.scan_run_id,
            value.path,
            value.error_code,
            value.summary
        ],
    )?;
    Ok(())
}

fn upsert_observation(connection: &Connection, value: &ObservationValues) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO observations(
            id, skill_id, adapter_id, scope, project_id, source_root_kind, source_root_id,
            display_path, normalized_path, canonical_path, deployment_name, digest, status,
            error_code, error_summary, last_successful_run_id, first_seen_at_ms,
            observed_at_ms, stale_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19
         )
         ON CONFLICT(id) DO UPDATE SET
            skill_id = coalesce(excluded.skill_id, observations.skill_id),
            project_id = excluded.project_id,
            source_root_kind = excluded.source_root_kind,
            source_root_id = excluded.source_root_id,
            display_path = excluded.display_path,
            normalized_path = excluded.normalized_path,
            canonical_path = excluded.canonical_path,
            deployment_name = excluded.deployment_name,
            digest = excluded.digest,
            status = excluded.status,
            error_code = excluded.error_code,
            error_summary = excluded.error_summary,
            last_successful_run_id = coalesce(
                excluded.last_successful_run_id,
                observations.last_successful_run_id
            ),
            observed_at_ms = excluded.observed_at_ms,
            stale_at_ms = excluded.stale_at_ms
         ON CONFLICT(adapter_id, scope, normalized_path) DO UPDATE SET
            skill_id = coalesce(excluded.skill_id, observations.skill_id),
            project_id = excluded.project_id,
            source_root_kind = excluded.source_root_kind,
            source_root_id = excluded.source_root_id,
            display_path = excluded.display_path,
            canonical_path = excluded.canonical_path,
            deployment_name = excluded.deployment_name,
            digest = excluded.digest,
            status = excluded.status,
            error_code = excluded.error_code,
            error_summary = excluded.error_summary,
            last_successful_run_id = coalesce(
                excluded.last_successful_run_id,
                observations.last_successful_run_id
            ),
            observed_at_ms = excluded.observed_at_ms,
            stale_at_ms = excluded.stale_at_ms",
        params![
            value.id,
            value.skill_id,
            value.adapter_id,
            value.scope,
            value.project_id,
            value.source_root_kind,
            value.source_root_id,
            value.display_path,
            value.normalized_path,
            value.canonical_path,
            value.deployment_name,
            value.digest,
            value.status,
            value.error_code,
            value.error_summary,
            value.last_successful_run_id,
            value.first_seen_at,
            value.observed_at,
            value.stale_at
        ],
    )?;
    Ok(())
}

fn upsert_operation(connection: &Connection, value: &OperationValues) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO operations(
            id, plan_digest, operation_type, state, outcome, recovery_state,
            journal_path, created_at_ms, updated_at_ms, finalized_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
            state = excluded.state,
            outcome = excluded.outcome,
            recovery_state = excluded.recovery_state,
            updated_at_ms = excluded.updated_at_ms,
            finalized_at_ms = excluded.finalized_at_ms",
        params![
            value.id,
            value.plan_digest,
            value.operation_type,
            value.state,
            value.outcome,
            value.recovery_state,
            value.journal_path,
            value.created_at,
            value.updated_at,
            value.finalized_at
        ],
    )?;
    Ok(())
}

fn insert_activity(connection: &Connection, value: &ActivityValues) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO activity(
            id, operation_id, scan_run_id, kind, state, outcome, summary, details_json,
            started_at_ms, completed_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
            state = excluded.state,
            outcome = excluded.outcome,
            summary = excluded.summary,
            details_json = excluded.details_json,
            completed_at_ms = excluded.completed_at_ms
         ON CONFLICT(scan_run_id) WHERE scan_run_id IS NOT NULL DO UPDATE SET
            state = excluded.state,
            outcome = excluded.outcome,
            summary = excluded.summary,
            details_json = excluded.details_json,
            completed_at_ms = excluded.completed_at_ms
         ON CONFLICT(operation_id) WHERE operation_id IS NOT NULL DO UPDATE SET
            state = excluded.state,
            outcome = excluded.outcome,
            summary = excluded.summary,
            details_json = excluded.details_json,
            completed_at_ms = excluded.completed_at_ms",
        params![
            value.id,
            value.operation_id,
            value.scan_run_id,
            value.kind,
            value.state,
            value.outcome,
            value.summary,
            value.details,
            value.started_at,
            value.completed_at
        ],
    )?;
    Ok(())
}

fn activity_list_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityListRecord> {
    Ok(ActivityListRecord {
        id: parse_text(&row.get::<_, String>(0)?, 0)?,
        kind: row.get(1)?,
        state: row.get(2)?,
        outcome: row.get(3)?,
        summary: row.get(4)?,
        started_at: parse_millis(row.get(5)?, 5)?,
        completed_at: row
            .get::<_, Option<i64>>(6)?
            .map(|v| parse_millis(v, 6))
            .transpose()?,
        operation_id: row
            .get::<_, Option<String>>(7)?
            .map(|v| parse_text(&v, 7))
            .transpose()?,
        scan_run_id: row
            .get::<_, Option<String>>(8)?
            .map(|v| parse_text(&v, 8))
            .transpose()?,
    })
}

fn activity_detail_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityDetailRecord> {
    let item = activity_list_from_row(row)?;
    let text: String = row.get(9)?;
    let details = serde_json::from_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(error))
    })?;
    Ok(ActivityDetailRecord { item, details })
}

fn persist_deployment(c: &Connection, p: &DeploymentProjection) -> rusqlite::Result<()> {
    upsert_operation(
        c,
        &OperationValues::try_from(p.operation.clone()).map_err(sql_error)?,
    )?;
    let d = &p.deployment;
    let changed = c.execute(
        "INSERT INTO deployments(
            id,skill_id,target_id,deployment_name,normalized_deployment_name,target_path,mode,
            expected_digest,expected_link_target,health,adapter_version,active,last_verified_at_ms,
            last_operation_id,created_at_ms,updated_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
         ON CONFLICT(id) DO UPDATE SET
            deployment_name=excluded.deployment_name,
            normalized_deployment_name=excluded.normalized_deployment_name,
            target_path=excluded.target_path, mode=excluded.mode,
            expected_digest=excluded.expected_digest,
            expected_link_target=excluded.expected_link_target, health=excluded.health,
            adapter_version=excluded.adapter_version, active=excluded.active,
            last_verified_at_ms=excluded.last_verified_at_ms,
            last_operation_id=excluded.last_operation_id, updated_at_ms=excluded.updated_at_ms
         WHERE deployments.skill_id=excluded.skill_id AND deployments.target_id=excluded.target_id",
        params![
            d.id.to_string(),
            d.skill_id.to_string(),
            d.target_id.to_string(),
            d.deployment_name.as_str(),
            d.deployment_name.collision_key(),
            path_sql(&d.target_path)?,
            deployment_mode_text(d.mode),
            d.expected_digest.to_string(),
            d.expected_link_target
                .as_deref()
                .map(path_sql)
                .transpose()?,
            deployment_health_text(d.health),
            d.adapter_version.to_string(),
            d.active,
            d.last_verified_at.map(ms).transpose()?,
            d.last_operation_id.map(|v| v.to_string()),
            ms(d.created_at)?,
            ms(d.updated_at)?
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    if let Some(s) = &p.snapshot {
        c.execute("INSERT INTO snapshots(id,operation_id,retention_state,protected,created_at_ms) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET retention_state=excluded.retention_state,protected=excluded.protected WHERE snapshots.operation_id=excluded.operation_id",params![s.id.to_string(),s.operation_id.to_string(),s.retention_state,s.protected,ms(s.created_at)?])?;
    }
    for i in &p.snapshot_items {
        c.execute("INSERT INTO snapshot_items(snapshot_id,ordinal,digest,entry_fingerprint_json,relation) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(snapshot_id,ordinal) DO UPDATE SET digest=excluded.digest,entry_fingerprint_json=excluded.entry_fingerprint_json,relation=excluded.relation",params![i.snapshot_id.to_string(),i64::try_from(i.ordinal).map_err(|_|sql_error(RepositoryError::IntegerOverflow))?,i.digest.map(|v|v.to_string()),i.entry_fingerprint.as_ref().map(json_text).transpose().map_err(sql_error)?,i.relation])?;
    }
    insert_activity(
        c,
        &ActivityValues::try_from(p.activity.clone()).map_err(sql_error)?,
    )
}

fn persist_takeover(c: &Connection, p: &TakeoverProjection) -> rusqlite::Result<()> {
    upsert_operation(
        c,
        &OperationValues::try_from(p.operation.clone()).map_err(sql_error)?,
    )?;
    c.execute("INSERT INTO skills(id,display_name,deployment_name,normalized_deployment_name,working_path,working_digest,baseline_digest,lifecycle,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,deployment_name=excluded.deployment_name,normalized_deployment_name=excluded.normalized_deployment_name,working_path=excluded.working_path,working_digest=excluded.working_digest,baseline_digest=excluded.baseline_digest,lifecycle=excluded.lifecycle,updated_at_ms=excluded.updated_at_ms", params![p.skill.id.to_string(),p.skill.display_name,p.skill.deployment_name.as_str(),p.skill.deployment_name.collision_key(),p.skill.working_path.to_string(),p.skill.working_digest.to_string(),p.skill.baseline_digest.to_string(),skill_lifecycle_text(p.skill.lifecycle),ms(p.skill.created_at)?,ms(p.skill.updated_at)?])?;
    c.execute("INSERT INTO objects(digest,relative_path,entry_count,byte_count,verified_at_ms) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(digest) DO UPDATE SET relative_path=excluded.relative_path,entry_count=excluded.entry_count,byte_count=excluded.byte_count,verified_at_ms=excluded.verified_at_ms",params![p.object.digest.to_string(),p.object.relative_path.to_string(),i64v(p.object.entry_count)?,i64v(p.object.byte_count)?,ms(p.object.verified_at)?])?;
    for s in &p.sources {
        c.execute("INSERT INTO skill_sources(skill_id,kind,source_path,captured_at_ms,confidence) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(skill_id,kind,source_path) DO UPDATE SET captured_at_ms=excluded.captured_at_ms,confidence=excluded.confidence",params![s.skill_id.to_string(),s.kind,path_sql(&s.path)?,ms(s.captured_at)?,s.confidence])?;
    }
    c.execute("INSERT INTO skill_revisions(skill_id,digest,revision_kind,operation_id,created_at_ms) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(skill_id,digest,revision_kind) DO NOTHING",params![p.revision.skill_id.to_string(),p.revision.digest.to_string(),p.revision.kind,p.revision.operation_id.map(|v|v.to_string()),ms(p.revision.created_at)?])?;
    for t in &p.targets {
        let persisted = c.execute(
            "INSERT INTO targets(id,adapter_id,scope,root_path,canonical_root_path,project_id,is_override,is_custom,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET updated_at_ms=excluded.updated_at_ms
             WHERE targets.adapter_id=excluded.adapter_id
               AND targets.scope=excluded.scope
               AND targets.root_path=excluded.root_path
               AND targets.canonical_root_path=excluded.canonical_root_path
               AND targets.project_id IS excluded.project_id
               AND targets.is_override=excluded.is_override
               AND targets.is_custom=excluded.is_custom",
            params![t.id.to_string(),t.adapter_id.to_string(),t.scope,path_sql(&t.root_path)?,path_sql(&t.canonical_root_path)?,t.project_id.map(|v|v.to_string()),t.is_override,t.is_custom,ms(t.created_at)?,ms(t.updated_at)?],
        )?;
        if persisted != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
    }
    for d in &p.deployments {
        c.execute("INSERT INTO deployments(id,skill_id,target_id,deployment_name,normalized_deployment_name,target_path,mode,expected_digest,expected_link_target,health,adapter_version,active,last_verified_at_ms,last_operation_id,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16) ON CONFLICT(id) DO UPDATE SET deployment_name=excluded.deployment_name,normalized_deployment_name=excluded.normalized_deployment_name,target_path=excluded.target_path,mode=excluded.mode,expected_digest=excluded.expected_digest,expected_link_target=excluded.expected_link_target,health=excluded.health,adapter_version=excluded.adapter_version,active=excluded.active,last_verified_at_ms=excluded.last_verified_at_ms,last_operation_id=excluded.last_operation_id,updated_at_ms=excluded.updated_at_ms",params![d.id.to_string(),d.skill_id.to_string(),d.target_id.to_string(),d.deployment_name.as_str(),d.deployment_name.collision_key(),path_sql(&d.target_path)?,deployment_mode_text(d.mode),d.expected_digest.to_string(),d.expected_link_target.as_deref().map(path_sql).transpose()?,deployment_health_text(d.health),d.adapter_version.to_string(),d.active,d.last_verified_at.map(ms).transpose()?,d.last_operation_id.map(|v|v.to_string()),ms(d.created_at)?,ms(d.updated_at)?])?;
    }
    if let Some(s) = &p.snapshot {
        c.execute("INSERT INTO snapshots(id,operation_id,retention_state,protected,created_at_ms) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET retention_state=excluded.retention_state,protected=excluded.protected",params![s.id.to_string(),s.operation_id.to_string(),s.retention_state,s.protected,ms(s.created_at)?])?;
    }
    for i in &p.snapshot_items {
        c.execute("INSERT INTO snapshot_items(snapshot_id,ordinal,digest,entry_fingerprint_json,relation) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(snapshot_id,ordinal) DO UPDATE SET digest=excluded.digest,entry_fingerprint_json=excluded.entry_fingerprint_json,relation=excluded.relation",params![i.snapshot_id.to_string(),i64::try_from(i.ordinal).map_err(|_|sql_error(RepositoryError::IntegerOverflow))?,i.digest.map(|v|v.to_string()),i.entry_fingerprint.as_ref().map(json_text).transpose().map_err(sql_error)?,i.relation])?;
    }
    for id in &p.observation_ids {
        let updated = c.execute(
            "UPDATE observations
             SET skill_id = ?1
             WHERE id = ?2 AND (skill_id IS NULL OR skill_id = ?1)",
            params![p.skill.id.to_string(), id.to_string()],
        )?;
        if updated != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
    }
    insert_activity(
        c,
        &ActivityValues::try_from(p.activity.clone()).map_err(sql_error)?,
    )
}

fn sql_error(error: RepositoryError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}
fn path_sql(path: &Path) -> rusqlite::Result<&str> {
    path.to_str()
        .ok_or_else(|| sql_error(RepositoryError::PathNotUtf8(path.to_path_buf())))
}
fn ms(value: UtcTimestamp) -> rusqlite::Result<i64> {
    millis(value).map_err(sql_error)
}
fn i64v(value: u64) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| sql_error(RepositoryError::IntegerOverflow))
}

fn path_text(path: &Path) -> Result<String, RepositoryError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| RepositoryError::PathNotUtf8(path.to_path_buf()))
}

fn json_text(value: &Value) -> Result<String, RepositoryError> {
    serde_json::to_string(value).map_err(RepositoryError::Json)
}

fn millis(value: UtcTimestamp) -> Result<i64, RepositoryError> {
    value.unix_millis().map_err(RepositoryError::Time)
}

fn parse_text<T>(value: &str, column: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value.parse().map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(source))
    })
}

fn parse_millis(value: i64, column: usize) -> rusqlite::Result<UtcTimestamp> {
    UtcTimestamp::from_unix_millis(value).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(source))
    })
}

fn parse_deployment_name(value: &str, column: usize) -> rusqlite::Result<DeploymentName> {
    DeploymentName::parse(value).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(column, Type::Text, Box::new(source))
    })
}

fn workspace_root_from_row(
    id: WorkspaceRootId,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WorkspaceRootRecord> {
    workspace_root_from_row_offset(id, row, 0)
}

fn authorization_identity_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AuthorizationIdentityRecord> {
    Ok(AuthorizationIdentityRecord {
        device_id: parse_text(&row.get::<_, String>(0)?, 0)?,
        file_id: parse_text(&row.get::<_, String>(1)?, 1)?,
    })
}

fn workspace_root_from_row_offset(
    id: WorkspaceRootId,
    row: &rusqlite::Row<'_>,
    o: usize,
) -> rusqlite::Result<WorkspaceRootRecord> {
    let ignore_rules = row.get::<_, String>(o + 4)?;
    Ok(WorkspaceRootRecord {
        id,
        selected_path: PathBuf::from(row.get::<_, String>(o)?),
        canonical_path: PathBuf::from(row.get::<_, String>(o + 1)?),
        paused: row.get(o + 2)?,
        maximum_depth: usize::try_from(row.get::<_, i64>(o + 3)?).map_err(|source| {
            rusqlite::Error::FromSqlConversionFailure(o + 3, Type::Integer, Box::new(source))
        })?,
        ignore_rules: serde_json::from_str(&ignore_rules).map_err(|source| {
            rusqlite::Error::FromSqlConversionFailure(o + 4, Type::Text, Box::new(source))
        })?,
        scan_status: row.get(o + 5)?,
        created_at: parse_millis(row.get(o + 6)?, o + 6)?,
        updated_at: parse_millis(row.get(o + 7)?, o + 7)?,
    })
}

fn project_from_row_offset(
    id: ProjectId,
    row: &rusqlite::Row<'_>,
    o: usize,
) -> rusqlite::Result<ProjectRecord> {
    Ok(ProjectRecord {
        id,
        workspace_root_id: row
            .get::<_, Option<String>>(o)?
            .map(|value| parse_text(&value, o))
            .transpose()?,
        root_path: PathBuf::from(row.get::<_, String>(o + 1)?),
        canonical_path: PathBuf::from(row.get::<_, String>(o + 2)?),
        discovery_evidence: row.get(o + 3)?,
        git_classification: row.get(o + 4)?,
        manual: row.get(o + 5)?,
        created_at: parse_millis(row.get(o + 6)?, o + 6)?,
        updated_at: parse_millis(row.get(o + 7)?, o + 7)?,
    })
}

fn workspace_scan_run_from_row(
    root_id: &str,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ScanRunRecord> {
    let coverage = row.get::<_, String>(3)?;
    Ok(ScanRunRecord {
        id: parse_text(&row.get::<_, String>(0)?, 0)?,
        root_kind: "workspace_root".to_owned(),
        root_id: Some(root_id.to_owned()),
        scope: row.get(1)?,
        state: row.get(2)?,
        coverage: serde_json::from_str(&coverage).map_err(|source| {
            rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(source))
        })?,
        started_at: parse_millis(row.get(4)?, 4)?,
        completed_at: row
            .get::<_, Option<i64>>(5)?
            .map(|value| parse_millis(value, 5))
            .transpose()?,
    })
}

fn observation_from_row(
    id: ObservationId,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ObservationRecord> {
    observation_from_row_offset(id, row, 0)
}

fn observation_from_row_offset(
    id: ObservationId,
    row: &rusqlite::Row<'_>,
    o: usize,
) -> rusqlite::Result<ObservationRecord> {
    Ok(ObservationRecord {
        id,
        skill_id: row
            .get::<_, Option<String>>(o)?
            .map(|v| parse_text(&v, o))
            .transpose()?,
        adapter_id: parse_text(&row.get::<_, String>(o + 1)?, o + 1)?,
        scope: row.get(o + 2)?,
        project_id: row
            .get::<_, Option<String>>(o + 3)?
            .map(|v| parse_text(&v, o + 3))
            .transpose()?,
        source_root_kind: row.get(o + 4)?,
        source_root_id: row.get(o + 5)?,
        display_path: PathBuf::from(row.get::<_, String>(o + 6)?),
        normalized_path: row.get(o + 7)?,
        canonical_path: row.get::<_, Option<String>>(o + 8)?.map(PathBuf::from),
        deployment_name: parse_deployment_name(&row.get::<_, String>(o + 9)?, o + 9)?,
        digest: row
            .get::<_, Option<String>>(o + 10)?
            .map(|v| parse_text(&v, o + 10))
            .transpose()?,
        status: row.get(o + 11)?,
        error_code: row.get(o + 12)?,
        error_summary: row.get(o + 13)?,
        last_successful_run_id: row
            .get::<_, Option<String>>(o + 14)?
            .map(|v| parse_text(&v, o + 14))
            .transpose()?,
        first_seen_at: parse_millis(row.get(o + 15)?, o + 15)?,
        observed_at: parse_millis(row.get(o + 16)?, o + 16)?,
        stale_at: row
            .get::<_, Option<i64>>(o + 17)?
            .map(|v| parse_millis(v, o + 17))
            .transpose()?,
    })
}

fn deployment_from_row(
    skill_id: SkillId,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<DeploymentRecord> {
    Ok(DeploymentRecord {
        id: parse_text(&row.get::<_, String>(0)?, 0)?,
        skill_id,
        target_id: parse_text(&row.get::<_, String>(1)?, 1)?,
        deployment_name: parse_deployment_name(&row.get::<_, String>(2)?, 2)?,
        target_path: PathBuf::from(row.get::<_, String>(3)?),
        mode: parse_deployment_mode(&row.get::<_, String>(4)?, 4)?,
        expected_digest: parse_text(&row.get::<_, String>(5)?, 5)?,
        expected_link_target: row.get::<_, Option<String>>(6)?.map(PathBuf::from),
        health: parse_deployment_health(&row.get::<_, String>(7)?, 7)?,
        adapter_version: parse_text(&row.get::<_, String>(8)?, 8)?,
        active: row.get(9)?,
        last_verified_at: row
            .get::<_, Option<i64>>(10)?
            .map(|v| parse_millis(v, 10))
            .transpose()?,
        last_operation_id: row
            .get::<_, Option<String>>(11)?
            .map(|v| parse_text(&v, 11))
            .transpose()?,
        created_at: parse_millis(row.get(12)?, 12)?,
        updated_at: parse_millis(row.get(13)?, 13)?,
    })
}

fn deployment_from_row_with_id(
    id: DeploymentId,
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<DeploymentRecord> {
    deployment_from_row_with_id_offset(id, row, 0)
}

fn deployment_from_row_with_id_offset(
    id: DeploymentId,
    row: &rusqlite::Row<'_>,
    o: usize,
) -> rusqlite::Result<DeploymentRecord> {
    Ok(DeploymentRecord {
        id,
        skill_id: parse_text(&row.get::<_, String>(o)?, o)?,
        target_id: parse_text(&row.get::<_, String>(o + 1)?, o + 1)?,
        deployment_name: parse_deployment_name(&row.get::<_, String>(o + 2)?, o + 2)?,
        target_path: PathBuf::from(row.get::<_, String>(o + 3)?),
        mode: parse_deployment_mode(&row.get::<_, String>(o + 4)?, o + 4)?,
        expected_digest: parse_text(&row.get::<_, String>(o + 5)?, o + 5)?,
        expected_link_target: row.get::<_, Option<String>>(o + 6)?.map(PathBuf::from),
        health: parse_deployment_health(&row.get::<_, String>(o + 7)?, o + 7)?,
        adapter_version: parse_text(&row.get::<_, String>(o + 8)?, o + 8)?,
        active: row.get(o + 9)?,
        last_verified_at: row
            .get::<_, Option<i64>>(o + 10)?
            .map(|v| parse_millis(v, o + 10))
            .transpose()?,
        last_operation_id: row
            .get::<_, Option<String>>(o + 11)?
            .map(|v| parse_text(&v, o + 11))
            .transpose()?,
        created_at: parse_millis(row.get(o + 12)?, o + 12)?,
        updated_at: parse_millis(row.get(o + 13)?, o + 13)?,
    })
}

fn target_from_row(id: TargetId, row: &rusqlite::Row<'_>) -> rusqlite::Result<TargetRecord> {
    target_from_row_offset(id, row, 0)
}

fn target_from_row_offset(
    id: TargetId,
    row: &rusqlite::Row<'_>,
    o: usize,
) -> rusqlite::Result<TargetRecord> {
    Ok(TargetRecord {
        id,
        adapter_id: parse_text(&row.get::<_, String>(o)?, o)?,
        scope: row.get(o + 1)?,
        root_path: PathBuf::from(row.get::<_, String>(o + 2)?),
        canonical_root_path: PathBuf::from(row.get::<_, String>(o + 3)?),
        project_id: row
            .get::<_, Option<String>>(o + 4)?
            .map(|v| parse_text(&v, o + 4))
            .transpose()?,
        is_override: row.get(o + 5)?,
        is_custom: row.get(o + 6)?,
        created_at: parse_millis(row.get(o + 7)?, o + 7)?,
        updated_at: parse_millis(row.get(o + 8)?, o + 8)?,
    })
}

fn invalid_value(value: &str, column: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(InvalidProjectionValue(value.to_owned())),
    )
}
fn parse_deployment_mode(value: &str, column: usize) -> rusqlite::Result<DeploymentMode> {
    match value {
        "symlink" => Ok(DeploymentMode::Symlink),
        "managed_copy" => Ok(DeploymentMode::ManagedCopy),
        _ => Err(invalid_value(value, column)),
    }
}
fn parse_deployment_health(value: &str, column: usize) -> rusqlite::Result<DeploymentHealth> {
    match value {
        "clean" => Ok(DeploymentHealth::Clean),
        "vault_ahead" => Ok(DeploymentHealth::VaultAhead),
        "target_modified" => Ok(DeploymentHealth::TargetModified),
        "missing_target" => Ok(DeploymentHealth::MissingTarget),
        "broken_link" => Ok(DeploymentHealth::BrokenLink),
        "conflict" => Ok(DeploymentHealth::Conflict),
        "unverified" => Ok(DeploymentHealth::Unverified),
        _ => Err(invalid_value(value, column)),
    }
}

fn skill_lifecycle_text(value: SkillLifecycle) -> &'static str {
    match value {
        SkillLifecycle::Active => "active",
        SkillLifecycle::Trashed => "trashed",
        SkillLifecycle::PermanentlyRemoved => "permanently_removed",
    }
}

fn parse_skill_lifecycle(value: &str, column: usize) -> rusqlite::Result<SkillLifecycle> {
    match value {
        "active" => Ok(SkillLifecycle::Active),
        "trashed" => Ok(SkillLifecycle::Trashed),
        "permanently_removed" => Ok(SkillLifecycle::PermanentlyRemoved),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            column,
            Type::Text,
            Box::new(InvalidProjectionValue(value.to_owned())),
        )),
    }
}

fn deployment_mode_text(value: DeploymentMode) -> &'static str {
    match value {
        DeploymentMode::Symlink => "symlink",
        DeploymentMode::ManagedCopy => "managed_copy",
    }
}

fn deployment_health_text(value: DeploymentHealth) -> &'static str {
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

fn operation_state_text(value: OperationState) -> &'static str {
    match value {
        OperationState::Planned => "planned",
        OperationState::Preflighted => "preflighted",
        OperationState::Snapshotted => "snapshotted",
        OperationState::Staged => "staged",
        OperationState::Committing => "committing",
        OperationState::Verifying => "verifying",
        OperationState::Committed => "committed",
        OperationState::Finalized => "finalized",
        OperationState::RollingBack => "rolling_back",
        OperationState::RolledBack => "rolled_back",
        OperationState::Failed => "failed",
        OperationState::RecoveryRequired => "recovery_required",
    }
}

fn operation_outcome_text(value: OperationOutcome) -> &'static str {
    match value {
        OperationOutcome::Succeeded => "succeeded",
        OperationOutcome::CancelledNoWrites => "cancelled_no_writes",
        OperationOutcome::FailedNoWrites => "failed_no_writes",
        OperationOutcome::FailedRolledBack => "failed_rolled_back",
        OperationOutcome::RecoveryRequired => "recovery_required",
    }
}

#[derive(Debug, Error)]
#[error("invalid persisted projection value: {0}")]
struct InvalidProjectionValue(String);

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("database executor failed: {0}")]
    Database(#[from] DbExecutorError),
    #[error("path cannot be represented in a readable SQLite/JSON contract: {0:?}")]
    PathNotUtf8(PathBuf),
    #[error("repository JSON serialization failed: {0}")]
    Json(serde_json::Error),
    #[error("repository timestamp failed: {0}")]
    Time(crate::domain::TimeError),
    #[error("repository integer exceeds SQLite range")]
    IntegerOverflow,
    #[error("invalid scan reconciliation: {0}")]
    InvalidScanReconciliation(&'static str),
    #[error("invalid takeover projection: {0}")]
    InvalidTakeoverProjection(&'static str),
    #[error("invalid deployment projection: {0}")]
    InvalidDeploymentProjection(&'static str),
    #[error("activity limit must be between 1 and 200")]
    InvalidActivityLimit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(value: i64) -> UtcTimestamp {
        UtcTimestamp::from_unix_millis(value).unwrap()
    }

    fn digest(byte: u8) -> BundleDigest {
        BundleDigest::from_bytes([byte; 32])
    }

    fn scan_reconciliation(
        adapter_id: &AdapterId,
        source_root_id: &str,
        names: &[&str],
        coverage_complete: bool,
        timestamp: i64,
    ) -> ScanReconciliation {
        let run_id = ScanRunId::generate();
        let observed_at = time(timestamp);
        ScanReconciliation {
            run: ScanRunRecord {
                id: run_id,
                root_kind: "adapter_global".to_owned(),
                root_id: Some(source_root_id.to_owned()),
                scope: "global".to_owned(),
                state: if coverage_complete {
                    "completed".to_owned()
                } else {
                    "cancelled".to_owned()
                },
                coverage: serde_json::json!({"complete": coverage_complete}),
                started_at: observed_at,
                completed_at: Some(observed_at),
            },
            adapter_id: adapter_id.clone(),
            scope: "global".to_owned(),
            source_root_kind: "adapter_global".to_owned(),
            source_root_id: source_root_id.to_owned(),
            observations: names
                .iter()
                .enumerate()
                .map(|(index, name)| ObservationRecord {
                    id: ObservationId::generate(),
                    skill_id: None,
                    adapter_id: adapter_id.clone(),
                    scope: "global".to_owned(),
                    project_id: None,
                    source_root_kind: "adapter_global".to_owned(),
                    source_root_id: source_root_id.to_owned(),
                    display_path: PathBuf::from(format!("/{source_root_id}/{name}")),
                    normalized_path: format!("/{source_root_id}/{name}"),
                    canonical_path: None,
                    deployment_name: DeploymentName::parse(*name).unwrap(),
                    digest: Some(digest(u8::try_from(index + 1).unwrap())),
                    status: "verified".to_owned(),
                    error_code: None,
                    error_summary: None,
                    last_successful_run_id: coverage_complete.then_some(run_id),
                    first_seen_at: observed_at,
                    observed_at,
                    stale_at: None,
                })
                .collect(),
            errors: Vec::new(),
            coverage_complete,
            activity: ActivityRecord {
                id: ActivityId::generate(),
                operation_id: None,
                kind: "scan".to_owned(),
                state: "completed".to_owned(),
                outcome: None,
                summary: "Scan completed".to_owned(),
                details: serde_json::json!({}),
                started_at: observed_at,
                completed_at: Some(observed_at),
            },
        }
    }

    #[test]
    fn reconciliation_is_root_scoped_idempotent_and_stales_only_after_complete_coverage() {
        let directory = tempfile::tempdir().unwrap();
        let repositories =
            Repositories::new(DbExecutor::open(directory.path().join("index.sqlite")).unwrap());
        let adapter_id: AdapterId = "universal-agent-skills@1".parse().unwrap();

        repositories
            .reconcile_scan(scan_reconciliation(
                &adapter_id,
                "root-a",
                &["alpha", "beta"],
                true,
                1_000,
            ))
            .unwrap();
        repositories
            .reconcile_scan(scan_reconciliation(
                &adapter_id,
                "root-b",
                &["gamma"],
                true,
                2_000,
            ))
            .unwrap();
        repositories
            .reconcile_scan(scan_reconciliation(
                &adapter_id,
                "root-a",
                &["alpha"],
                false,
                3_000,
            ))
            .unwrap();

        assert_eq!(repositories.external_observations().unwrap().len(), 3);

        assert_eq!(
            repositories
                .reconcile_scan(scan_reconciliation(
                    &adapter_id,
                    "root-a",
                    &["alpha"],
                    true,
                    4_000,
                ))
                .unwrap(),
            1
        );
        let active = repositories.external_observations().unwrap();
        assert_eq!(active.len(), 2);
        assert!(
            active
                .iter()
                .any(|record| record.deployment_name.as_str() == "gamma")
        );
        assert_eq!(
            active
                .iter()
                .find(|record| record.deployment_name.as_str() == "alpha")
                .unwrap()
                .first_seen_at,
            time(1_000)
        );

        assert_eq!(
            repositories
                .reconcile_scan(scan_reconciliation(
                    &adapter_id,
                    "root-a",
                    &["alpha"],
                    true,
                    5_000,
                ))
                .unwrap(),
            0
        );
        assert_eq!(repositories.external_observations().unwrap().len(), 2);
        assert_eq!(repositories.table_count("observations").unwrap(), 3);
    }

    #[test]
    fn reconciliation_rejects_a_scan_run_from_another_coverage_root() {
        let directory = tempfile::tempdir().unwrap();
        let repositories =
            Repositories::new(DbExecutor::open(directory.path().join("index.sqlite")).unwrap());
        let adapter_id: AdapterId = "universal-agent-skills@1".parse().unwrap();
        let mut reconciliation =
            scan_reconciliation(&adapter_id, "root-a", &["alpha"], true, 1_000);
        reconciliation.run.root_id = Some("root-b".to_owned());

        assert!(matches!(
            repositories.reconcile_scan(reconciliation),
            Err(RepositoryError::InvalidScanReconciliation(_))
        ));
        assert_eq!(repositories.table_count("scan_runs").unwrap(), 0);
        assert_eq!(repositories.table_count("observations").unwrap(), 0);
    }

    #[test]
    fn scan_activity_aggregates_diagnostics_once_on_reconciliation_replay() {
        let directory = tempfile::tempdir().unwrap();
        let repositories =
            Repositories::new(DbExecutor::open(directory.path().join("index.sqlite")).unwrap());
        let adapter_id: AdapterId = "universal-agent-skills@1".parse().unwrap();
        let mut scan = scan_reconciliation(&adapter_id, "root-a", &[], false, 1_000);
        scan.run.state = "completed_with_errors".to_owned();
        scan.errors = ["permission_denied", "invalid_bundle"]
            .into_iter()
            .map(|code| ScanErrorRecord {
                scan_run_id: scan.run.id,
                path: PathBuf::from(format!("/redacted/{code}")),
                error_code: code.to_owned(),
                summary: "Could not inspect candidate".to_owned(),
            })
            .collect();
        scan.activity.state = scan.run.state.clone();
        scan.activity.summary = "Scan finished with 2 diagnostics".to_owned();
        scan.activity.details = serde_json::json!({
            "diagnosticCount": 2,
            "errorCodes": ["permission_denied", "invalid_bundle"]
        });

        repositories.reconcile_scan(scan.clone()).unwrap();
        let mut reconstructed = scan;
        reconstructed.activity.id = ActivityId::generate();
        repositories.reconcile_scan(reconstructed).unwrap();

        assert_eq!(repositories.table_count("activity").unwrap(), 1);
        assert_eq!(repositories.table_count("scan_errors").unwrap(), 2);
        let item = repositories
            .activity_list(ActivityQuery {
                kind: Some("scan".to_owned()),
                outcome: None,
                limit: 10,
            })
            .unwrap()
            .pop()
            .unwrap();
        let detail = repositories.activity_detail(item.id).unwrap().unwrap();
        assert_eq!(detail.details["diagnosticCount"], 2);
    }

    #[test]
    fn reconciliation_keeps_case_distinct_paths_as_separate_observations() {
        let directory = tempfile::tempdir().unwrap();
        let repositories =
            Repositories::new(DbExecutor::open(directory.path().join("index.sqlite")).unwrap());
        let adapter_id: AdapterId = "universal-agent-skills@1".parse().unwrap();

        repositories
            .reconcile_scan(scan_reconciliation(
                &adapter_id,
                "case-sensitive-root",
                &["Alpha", "alpha"],
                true,
                1_000,
            ))
            .unwrap();

        let observations = repositories.external_observations().unwrap();
        assert_eq!(observations.len(), 2);
        assert_ne!(observations[0].display_path, observations[1].display_path);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn repositories_cover_the_initial_projection_without_content_blobs() {
        let directory = tempfile::tempdir().unwrap();
        let repositories =
            Repositories::new(DbExecutor::open(directory.path().join("index.sqlite")).unwrap());
        let skill_id = SkillId::generate();
        let operation_id = OperationId::generate();
        let workspace_id = WorkspaceRootId::generate();
        let project_id = ProjectId::generate();
        let target_id = TargetId::generate();
        let scan_id = ScanRunId::generate();
        let snapshot_id = SnapshotId::generate();
        let now = time(1_000);
        let skill = SkillRecord {
            id: skill_id,
            display_name: "Same Name".to_owned(),
            deployment_name: DeploymentName::parse("same-name").unwrap(),
            working_path: BundleRelativePath::parse(&format!("skills/{skill_id}/same-name"))
                .unwrap(),
            working_digest: digest(1),
            baseline_digest: digest(1),
            lifecycle: SkillLifecycle::Active,
            created_at: now,
            updated_at: now,
        };
        repositories.upsert_skill(skill.clone()).unwrap();
        repositories
            .insert_skill_source(SkillSourceRecord {
                skill_id,
                kind: "local-observation".to_owned(),
                path: directory.path().join("external"),
                captured_at: now,
                confidence: "observed".to_owned(),
            })
            .unwrap();
        repositories
            .upsert_object(ObjectRecord {
                digest: digest(1),
                relative_path: BundleRelativePath::parse(
                    ".manager/objects/sha256-bundle-v1/00/object",
                )
                .unwrap(),
                entry_count: 1,
                byte_count: 5,
                verified_at: now,
            })
            .unwrap();
        repositories
            .upsert_operation(OperationRecord {
                id: operation_id,
                plan_digest: "sha256-plan-v1:test".to_owned(),
                operation_type: "takeover".to_owned(),
                state: OperationState::Planned,
                outcome: None,
                recovery_state: None,
                journal_path: BundleRelativePath::parse(&format!(
                    ".manager/operations/{operation_id}/journal.json"
                ))
                .unwrap(),
                created_at: now,
                updated_at: now,
                finalized_at: None,
            })
            .unwrap();
        repositories
            .insert_skill_revision(SkillRevisionRecord {
                skill_id,
                digest: digest(1),
                kind: "takeover_baseline".to_owned(),
                operation_id: Some(operation_id),
                created_at: now,
            })
            .unwrap();
        repositories
            .upsert_workspace_root(WorkspaceRootRecord {
                id: workspace_id,
                selected_path: directory.path().join("workspace"),
                canonical_path: directory.path().join("workspace"),
                paused: false,
                maximum_depth: 12,
                ignore_rules: serde_json::json!(["node_modules"]),
                scan_status: "idle".to_owned(),
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        repositories
            .upsert_project(ProjectRecord {
                id: project_id,
                workspace_root_id: Some(workspace_id),
                root_path: directory.path().join("workspace/project"),
                canonical_path: directory.path().join("workspace/project"),
                discovery_evidence: "manual".to_owned(),
                git_classification: "git".to_owned(),
                manual: true,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        repositories
            .upsert_target(TargetRecord {
                id: target_id,
                adapter_id: "claude-code@1".parse().unwrap(),
                scope: "project".to_owned(),
                root_path: directory.path().join("workspace/project/.claude/skills"),
                canonical_root_path: directory.path().join("workspace/project/.claude/skills"),
                project_id: Some(project_id),
                is_override: false,
                is_custom: false,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        repositories
            .upsert_scan_run(ScanRunRecord {
                id: scan_id,
                root_kind: "target".to_owned(),
                root_id: Some(target_id.to_string()),
                scope: "project".to_owned(),
                state: "completed".to_owned(),
                coverage: serde_json::json!({"complete": true}),
                started_at: now,
                completed_at: Some(now),
            })
            .unwrap();
        repositories
            .append_scan_error(ScanErrorRecord {
                scan_run_id: scan_id,
                path: directory.path().join("unreadable"),
                error_code: "permission_denied".to_owned(),
                summary: "Skipped one path".to_owned(),
            })
            .unwrap();
        repositories
            .upsert_observation(ObservationRecord {
                id: ObservationId::generate(),
                skill_id: Some(skill_id),
                adapter_id: "claude-code@1".parse().unwrap(),
                scope: "project".to_owned(),
                project_id: Some(project_id),
                source_root_kind: "target".to_owned(),
                source_root_id: target_id.to_string(),
                display_path: directory.path().join("external/same-name"),
                normalized_path: "/external/same-name".to_owned(),
                canonical_path: None,
                deployment_name: DeploymentName::parse("same-name").unwrap(),
                digest: Some(digest(1)),
                status: "verified".to_owned(),
                error_code: None,
                error_summary: None,
                last_successful_run_id: Some(scan_id),
                first_seen_at: now,
                observed_at: now,
                stale_at: None,
            })
            .unwrap();
        repositories
            .upsert_deployment(DeploymentRecord {
                id: DeploymentId::generate(),
                skill_id,
                target_id,
                deployment_name: DeploymentName::parse("same-name").unwrap(),
                target_path: directory.path().join("target/same-name"),
                mode: DeploymentMode::ManagedCopy,
                expected_digest: digest(1),
                expected_link_target: None,
                health: DeploymentHealth::Clean,
                adapter_version: "claude-code@1".parse().unwrap(),
                active: true,
                last_verified_at: Some(now),
                last_operation_id: Some(operation_id),
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        repositories
            .upsert_operation_step(OperationStepRecord {
                operation_id,
                ordinal: 0,
                action: "stage".to_owned(),
                precondition: serde_json::json!({}),
                staging_path: Some(directory.path().join("stage")),
                backup_path: None,
                state: "completed".to_owned(),
                result: Some(serde_json::json!({"verified": true})),
            })
            .unwrap();
        repositories
            .upsert_snapshot(SnapshotRecord {
                id: snapshot_id,
                operation_id,
                retention_state: "retained".to_owned(),
                protected: true,
                created_at: now,
            })
            .unwrap();
        repositories
            .upsert_snapshot_item(SnapshotItemRecord {
                snapshot_id,
                ordinal: 0,
                digest: Some(digest(1)),
                entry_fingerprint: None,
                relation: "target_before".to_owned(),
            })
            .unwrap();
        repositories
            .append_activity(ActivityRecord {
                id: ActivityId::generate(),
                operation_id: Some(operation_id),
                kind: "takeover".to_owned(),
                state: "completed".to_owned(),
                outcome: Some(OperationOutcome::Succeeded),
                summary: "Added Skill".to_owned(),
                details: serde_json::json!({}),
                started_at: now,
                completed_at: Some(now),
            })
            .unwrap();
        repositories
            .set_setting(
                "trash.retention".to_owned(),
                &serde_json::json!("explicit"),
                now,
            )
            .unwrap();

        assert_eq!(repositories.skill(skill_id).unwrap(), Some(skill));
        for table in [
            "skills",
            "skill_sources",
            "objects",
            "skill_revisions",
            "workspace_roots",
            "projects",
            "targets",
            "scan_runs",
            "scan_errors",
            "observations",
            "deployments",
            "operations",
            "operation_steps",
            "snapshots",
            "snapshot_items",
            "activity",
            "settings",
        ] {
            assert_eq!(repositories.table_count(table).unwrap(), 1, "table {table}");
        }
        assert!(repositories.content_blob_columns().unwrap().is_empty());
    }

    #[test]
    fn foreign_keys_reject_orphaned_deployments() {
        let directory = tempfile::tempdir().unwrap();
        let repositories =
            Repositories::new(DbExecutor::open(directory.path().join("index.sqlite")).unwrap());
        let now = time(1_000);
        let result = repositories.upsert_deployment(DeploymentRecord {
            id: DeploymentId::generate(),
            skill_id: SkillId::generate(),
            target_id: TargetId::generate(),
            deployment_name: DeploymentName::parse("orphan").unwrap(),
            target_path: directory.path().join("orphan"),
            mode: DeploymentMode::ManagedCopy,
            expected_digest: digest(1),
            expected_link_target: None,
            health: DeploymentHealth::Unverified,
            adapter_version: "claude-code@1".parse().unwrap(),
            active: true,
            last_verified_at: None,
            last_operation_id: None,
            created_at: now,
            updated_at: now,
        });

        assert!(result.is_err());
        assert_eq!(repositories.table_count("deployments").unwrap(), 0);
    }

    #[test]
    fn critical_finalization_is_atomic_and_restores_normal_synchronization() {
        let directory = tempfile::tempdir().unwrap();
        let database = DbExecutor::open(directory.path().join("index.sqlite")).unwrap();
        let repositories = Repositories::new(database.clone());
        let operation_id = OperationId::generate();
        let now = time(1_000);
        let operation = OperationRecord {
            id: operation_id,
            plan_digest: "sha256-plan-v1:test".to_owned(),
            operation_type: "takeover".to_owned(),
            state: OperationState::Finalized,
            outcome: Some(OperationOutcome::Succeeded),
            recovery_state: None,
            journal_path: BundleRelativePath::parse(&format!(
                ".manager/operations/{operation_id}/journal.json"
            ))
            .unwrap(),
            created_at: now,
            updated_at: now,
            finalized_at: Some(now),
        };
        let activity = ActivityRecord {
            id: ActivityId::generate(),
            operation_id: Some(OperationId::generate()),
            kind: "takeover".to_owned(),
            state: "completed".to_owned(),
            outcome: Some(OperationOutcome::Succeeded),
            summary: "Should roll back".to_owned(),
            details: serde_json::json!({}),
            started_at: now,
            completed_at: Some(now),
        };

        assert!(
            repositories
                .finalize_operation(operation, activity)
                .is_err()
        );
        assert_eq!(repositories.table_count("operations").unwrap(), 0);
        assert_eq!(repositories.table_count("activity").unwrap(), 0);
        assert_eq!(database.settings().unwrap().synchronous, 1);
    }
}
