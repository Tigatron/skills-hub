CREATE TABLE observations_v2 (
    id TEXT PRIMARY KEY NOT NULL,
    skill_id TEXT REFERENCES skills(id) ON DELETE SET NULL,
    adapter_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
    source_root_kind TEXT NOT NULL,
    source_root_id TEXT NOT NULL,
    display_path TEXT NOT NULL,
    normalized_path TEXT NOT NULL,
    canonical_path TEXT,
    deployment_name TEXT NOT NULL,
    digest TEXT,
    status TEXT NOT NULL,
    error_code TEXT,
    error_summary TEXT,
    last_successful_run_id TEXT REFERENCES scan_runs(id) ON DELETE SET NULL,
    first_seen_at_ms INTEGER NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    stale_at_ms INTEGER,
    UNIQUE (adapter_id, scope, normalized_path)
) STRICT;

INSERT INTO observations_v2(
    id, skill_id, adapter_id, scope, project_id, source_root_kind, source_root_id,
    display_path, normalized_path, canonical_path, deployment_name, digest, status,
    error_code, error_summary, last_successful_run_id, first_seen_at_ms,
    observed_at_ms, stale_at_ms
)
SELECT
    observation.id,
    observation.skill_id,
    observation.adapter_id,
    observation.scope,
    observation.project_id,
    coalesce(run.root_kind, 'legacy'),
    coalesce(run.root_id, 'legacy:' || observation.adapter_id || ':' || observation.scope),
    observation.display_path,
    observation.normalized_path,
    observation.canonical_path,
    observation.deployment_name,
    observation.digest,
    observation.status,
    NULL,
    NULL,
    observation.last_successful_run_id,
    observation.observed_at_ms,
    observation.observed_at_ms,
    NULL
FROM observations AS observation
LEFT JOIN scan_runs AS run ON run.id = observation.last_successful_run_id;

DROP TABLE observations;
ALTER TABLE observations_v2 RENAME TO observations;

CREATE INDEX observations_skill_id ON observations(skill_id);
CREATE INDEX observations_digest ON observations(digest) WHERE digest IS NOT NULL;
CREATE INDEX observations_coverage_root
    ON observations(adapter_id, scope, source_root_kind, source_root_id, status);
