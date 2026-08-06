-- M0-016 library and deployment query indexes at reference scale.
CREATE INDEX IF NOT EXISTS observations_external_active
    ON observations(deployment_name, normalized_path, id)
    WHERE skill_id IS NULL AND status <> 'stale';

CREATE INDEX IF NOT EXISTS observations_normalized_name
    ON observations(deployment_name)
    WHERE status <> 'stale';

CREATE INDEX IF NOT EXISTS skills_active_name
    ON skills(normalized_deployment_name, id)
    WHERE lifecycle = 'active';

CREATE INDEX IF NOT EXISTS deployments_active_by_skill
    ON deployments(skill_id, id)
    WHERE active = 1;
