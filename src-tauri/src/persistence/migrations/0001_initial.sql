CREATE TABLE skills (
    id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL CHECK (length(display_name) > 0),
    deployment_name TEXT NOT NULL CHECK (length(deployment_name) > 0),
    normalized_deployment_name TEXT NOT NULL CHECK (length(normalized_deployment_name) > 0),
    working_path TEXT NOT NULL UNIQUE,
    working_digest TEXT NOT NULL CHECK (length(working_digest) = 81),
    baseline_digest TEXT NOT NULL CHECK (length(baseline_digest) = 81),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'trashed', 'permanently_removed')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE skill_sources (
    id INTEGER PRIMARY KEY,
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    source_path TEXT NOT NULL,
    captured_at_ms INTEGER NOT NULL,
    confidence TEXT NOT NULL,
    UNIQUE (skill_id, kind, source_path)
) STRICT;

CREATE TABLE objects (
    digest TEXT PRIMARY KEY NOT NULL CHECK (length(digest) = 81),
    relative_path TEXT NOT NULL UNIQUE,
    entry_count INTEGER NOT NULL CHECK (entry_count >= 0),
    byte_count INTEGER NOT NULL CHECK (byte_count >= 0),
    verified_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE operations (
    id TEXT PRIMARY KEY NOT NULL,
    plan_digest TEXT NOT NULL,
    operation_type TEXT NOT NULL,
    state TEXT NOT NULL,
    outcome TEXT,
    recovery_state TEXT,
    journal_path TEXT NOT NULL UNIQUE,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    finalized_at_ms INTEGER
) STRICT;

CREATE TABLE skill_revisions (
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    digest TEXT NOT NULL REFERENCES objects(digest) ON DELETE RESTRICT,
    revision_kind TEXT NOT NULL,
    operation_id TEXT REFERENCES operations(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (skill_id, digest, revision_kind)
) STRICT;

CREATE TABLE workspace_roots (
    id TEXT PRIMARY KEY NOT NULL,
    selected_path TEXT NOT NULL,
    canonical_path TEXT NOT NULL UNIQUE,
    paused INTEGER NOT NULL CHECK (paused IN (0, 1)),
    maximum_depth INTEGER NOT NULL CHECK (maximum_depth > 0),
    ignore_rules_json TEXT NOT NULL CHECK (json_valid(ignore_rules_json)),
    scan_status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE projects (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_root_id TEXT REFERENCES workspace_roots(id) ON DELETE SET NULL,
    root_path TEXT NOT NULL,
    canonical_path TEXT NOT NULL UNIQUE,
    discovery_evidence TEXT NOT NULL,
    git_classification TEXT NOT NULL,
    manual INTEGER NOT NULL CHECK (manual IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE targets (
    id TEXT PRIMARY KEY NOT NULL,
    adapter_id TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('global', 'project', 'custom')),
    root_path TEXT NOT NULL,
    canonical_root_path TEXT NOT NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    is_override INTEGER NOT NULL CHECK (is_override IN (0, 1)),
    is_custom INTEGER NOT NULL CHECK (is_custom IN (0, 1)),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX targets_stable_identity
    ON targets(adapter_id, scope, canonical_root_path, coalesce(project_id, ''));

CREATE TABLE scan_runs (
    id TEXT PRIMARY KEY NOT NULL,
    root_kind TEXT NOT NULL,
    root_id TEXT,
    scope TEXT NOT NULL,
    state TEXT NOT NULL,
    coverage_json TEXT NOT NULL CHECK (json_valid(coverage_json)),
    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER
) STRICT;

CREATE TABLE scan_errors (
    id INTEGER PRIMARY KEY,
    scan_run_id TEXT NOT NULL REFERENCES scan_runs(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    error_code TEXT NOT NULL,
    summary TEXT NOT NULL
) STRICT;

CREATE TABLE observations (
    id TEXT PRIMARY KEY NOT NULL,
    skill_id TEXT REFERENCES skills(id) ON DELETE SET NULL,
    adapter_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    display_path TEXT NOT NULL,
    normalized_path TEXT NOT NULL,
    canonical_path TEXT,
    deployment_name TEXT NOT NULL,
    digest TEXT,
    status TEXT NOT NULL,
    last_successful_run_id TEXT REFERENCES scan_runs(id) ON DELETE SET NULL,
    observed_at_ms INTEGER NOT NULL,
    UNIQUE (adapter_id, scope, normalized_path)
) STRICT;

CREATE INDEX observations_skill_id ON observations(skill_id);
CREATE INDEX observations_digest ON observations(digest) WHERE digest IS NOT NULL;

CREATE TABLE deployments (
    id TEXT PRIMARY KEY NOT NULL,
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE RESTRICT,
    target_id TEXT NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    deployment_name TEXT NOT NULL,
    normalized_deployment_name TEXT NOT NULL,
    target_path TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('symlink', 'managed_copy')),
    expected_digest TEXT NOT NULL CHECK (length(expected_digest) = 81),
    expected_link_target TEXT,
    health TEXT NOT NULL,
    adapter_version TEXT NOT NULL,
    active INTEGER NOT NULL CHECK (active IN (0, 1)),
    last_verified_at_ms INTEGER,
    last_operation_id TEXT REFERENCES operations(id) ON DELETE RESTRICT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX deployments_active_target_name
    ON deployments(target_id, normalized_deployment_name)
    WHERE active = 1;

CREATE TABLE operation_steps (
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    action TEXT NOT NULL,
    precondition_json TEXT NOT NULL CHECK (json_valid(precondition_json)),
    staging_path TEXT,
    backup_path TEXT,
    state TEXT NOT NULL,
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    PRIMARY KEY (operation_id, ordinal)
) STRICT;

CREATE TABLE snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE RESTRICT,
    retention_state TEXT NOT NULL,
    protected INTEGER NOT NULL CHECK (protected IN (0, 1)),
    created_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE snapshot_items (
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    digest TEXT REFERENCES objects(digest) ON DELETE RESTRICT,
    entry_fingerprint_json TEXT CHECK (
        entry_fingerprint_json IS NULL OR json_valid(entry_fingerprint_json)
    ),
    relation TEXT NOT NULL,
    PRIMARY KEY (snapshot_id, ordinal),
    CHECK (digest IS NOT NULL OR entry_fingerprint_json IS NOT NULL)
) STRICT;

CREATE TABLE activity (
    id TEXT PRIMARY KEY NOT NULL,
    operation_id TEXT REFERENCES operations(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    outcome TEXT,
    summary TEXT NOT NULL,
    details_json TEXT NOT NULL CHECK (json_valid(details_json)),
    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER
) STRICT;

CREATE INDEX activity_started_at ON activity(started_at_ms DESC);

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    updated_at_ms INTEGER NOT NULL
) STRICT;
