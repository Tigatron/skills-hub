CREATE TABLE workspace_root_identities (
    workspace_root_id TEXT PRIMARY KEY NOT NULL
        REFERENCES workspace_roots(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    file_id TEXT NOT NULL
) STRICT;

CREATE TABLE manual_project_identities (
    project_id TEXT PRIMARY KEY NOT NULL
        REFERENCES projects(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    file_id TEXT NOT NULL
) STRICT;
