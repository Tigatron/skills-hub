CREATE TABLE adapter_configurations (
    adapter_name TEXT PRIMARY KEY NOT NULL,
    adapter_id TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    global_override_path TEXT,
    project_override_path TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE target_registration_metadata (
    target_id TEXT PRIMARY KEY NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    preferred_mode TEXT CHECK (preferred_mode IS NULL OR preferred_mode IN ('symlink', 'managed_copy')),
    root_device_id TEXT NOT NULL,
    root_file_id TEXT NOT NULL,
    override_kind TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
) STRICT;
