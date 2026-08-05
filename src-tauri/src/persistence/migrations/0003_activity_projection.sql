ALTER TABLE activity ADD COLUMN scan_run_id TEXT REFERENCES scan_runs(id) ON DELETE SET NULL;

CREATE UNIQUE INDEX activity_scan_run_identity
    ON activity(scan_run_id) WHERE scan_run_id IS NOT NULL;
CREATE UNIQUE INDEX activity_operation_identity
    ON activity(operation_id) WHERE operation_id IS NOT NULL;
CREATE INDEX activity_bounded_query
    ON activity(kind, outcome, started_at_ms DESC, id DESC);
