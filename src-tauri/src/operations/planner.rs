use serde::{Deserialize, Serialize};

use crate::domain::{DeploymentId, OperationId, SkillId, TargetId, UtcTimestamp};

use super::{
    CancellationToken, OperationError, OperationKind, OperationPlan, OperationPlanContent,
    OperationStore, OwnershipChoice,
};

/// Path-free command accepted by the generic planner boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationIntent {
    pub operation_id: OperationId,
    pub kind: OperationKind,
    pub selected_skill_ids: Vec<SkillId>,
    pub selected_target_ids: Vec<TargetId>,
    pub selected_deployment_ids: Vec<DeploymentId>,
    pub ownership_choices: Vec<OwnershipChoice>,
}

/// Operation-kind code resolves domain IDs and authorized roots into frozen plan content.
pub trait PlanBuilder: Send + Sync {
    /// Builds content without mutating a target path.
    ///
    /// # Errors
    ///
    /// Returns a typed operation error for invalid IDs, blockers, cancellation, or inspection.
    fn build_content(
        &self,
        intent: &OperationIntent,
        cancellation: &CancellationToken,
    ) -> Result<OperationPlanContent, OperationError>;
}

/// Persists the exact immutable plan generated from a path-free intent.
#[derive(Debug, Clone)]
pub struct OperationPlanner {
    store: OperationStore,
}

impl OperationPlanner {
    #[must_use]
    pub const fn new(store: OperationStore) -> Self {
        Self { store }
    }

    /// Builds, validates, and durably persists one immutable reviewed plan.
    ///
    /// # Errors
    ///
    /// Returns an error when planning is cancelled, the builder changes the intent identity,
    /// canonical validation fails, or durable persistence fails.
    pub fn plan(
        &self,
        intent: &OperationIntent,
        builder: &dyn PlanBuilder,
        cancellation: &CancellationToken,
    ) -> Result<OperationPlan, OperationError> {
        cancellation.check()?;
        let content = builder.build_content(intent, cancellation)?;
        cancellation.check()?;
        if content.operation_id != intent.operation_id || content.kind != intent.kind {
            return Err(OperationError::InvalidPlan(
                "plan builder changed the Operation identity or kind".to_owned(),
            ));
        }
        let plan = OperationPlan::build(content)
            .map_err(|error| OperationError::InvalidPlan(error.to_string()))?;
        let mut selected_skill_ids = intent.selected_skill_ids.clone();
        selected_skill_ids.sort_unstable();
        selected_skill_ids.dedup();
        let mut selected_target_ids = intent.selected_target_ids.clone();
        selected_target_ids.sort_unstable();
        selected_target_ids.dedup();
        let mut selected_deployment_ids = intent.selected_deployment_ids.clone();
        selected_deployment_ids.sort_unstable();
        selected_deployment_ids.dedup();
        let mut ownership_choices = intent.ownership_choices.clone();
        ownership_choices.sort_unstable();
        if plan.content.selected_skill_ids != selected_skill_ids
            || plan.content.selected_target_ids != selected_target_ids
            || plan.content.selected_deployment_ids != selected_deployment_ids
            || plan.content.ownership_choices != ownership_choices
        {
            return Err(OperationError::InvalidPlan(
                "plan builder changed the intent's selected domain IDs or choices".to_owned(),
            ));
        }
        self.store
            .persist_new_plan(&plan, UtcTimestamp::now())
            .map_err(OperationError::Journal)?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::{
        domain::{AdapterId, BundleRelativePath, DurationMillis},
        filesystem::{AuthorizedRoot, BundleCaps, BundleStats, EntryKind},
        operations::{PathFingerprint, PlanAction, PlanPath, PlanStep, RecoverySummary},
    };

    struct StaticBuilder(OperationPlanContent);

    impl PlanBuilder for StaticBuilder {
        fn build_content(
            &self,
            _intent: &OperationIntent,
            _cancellation: &CancellationToken,
        ) -> Result<OperationPlanContent, OperationError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn planner_persists_review_evidence_without_writing_target() {
        let temporary = tempdir().unwrap();
        let manager = temporary.path().join(".manager");
        let target = temporary.path().join("target");
        fs::create_dir(&manager).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("user.txt"), b"unchanged").unwrap();
        let store = OperationStore::open(&manager).unwrap();
        let operation_id = OperationId::generate();
        let target_id = TargetId::generate();
        let created_at = UtcTimestamp::now();
        let expires_at = created_at.checked_add(DurationMillis(60_000)).unwrap();
        let root = AuthorizedRoot::open(&target).unwrap();
        let authorized = root
            .authorize(&BundleRelativePath::parse("untouched").unwrap())
            .unwrap();
        let fingerprint = PathFingerprint {
            expected_kind: EntryKind::Absent,
            raw_symlink_target: None,
            metadata: None,
            bundle_digest: None,
            bundle_subpath: None,
            resolved_bundle_digest: None,
            managed_skill_id: None,
            managed_deployment_id: None,
            captured_at: created_at,
            adapter_id: AdapterId::new("planner-test", 1).unwrap(),
        };
        let content = OperationPlanContent::new(
            operation_id,
            OperationKind::Deploy,
            created_at,
            expires_at,
            Vec::new(),
            vec![target_id],
            Vec::new(),
            Vec::new(),
            BundleCaps::default(),
            BundleStats::default(),
            vec![PlanStep::new(
                PlanAction::LeaveUntouched,
                PlanPath::from_authorized(target_id, &authorized).unwrap(),
                None,
                None,
                fingerprint.clone(),
                fingerprint,
                false,
            )],
            Vec::new(),
            RecoverySummary {
                snapshot_count: 0,
                estimated_staging_bytes: 0,
                estimated_snapshot_bytes: 0,
                estimated_rollback_bytes: 0,
                spans_filesystems: false,
            },
            Vec::new(),
        );
        let intent = OperationIntent {
            operation_id,
            kind: OperationKind::Deploy,
            selected_skill_ids: Vec::new(),
            selected_target_ids: vec![target_id],
            selected_deployment_ids: Vec::new(),
            ownership_choices: Vec::new(),
        };
        let planner = OperationPlanner::new(store.clone());

        let plan = planner
            .plan(
                &intent,
                &StaticBuilder(content),
                &CancellationToken::default(),
            )
            .unwrap();

        assert_eq!(fs::read(target.join("user.txt")).unwrap(), b"unchanged");
        assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
        assert_eq!(store.load(operation_id).unwrap().plan, plan);
    }
}
