use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::OperationState;

use super::{
    OperationError, PhaseStatus, PlanAction, StoredOperation, TargetRoots,
    executor::{
        ArtifactKind, artifact_is_owned, capture_plan_path, capture_raw_path, fingerprint_matches,
    },
};

/// Deterministic startup action selected from durable evidence plus actual path fingerprints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupDecision {
    AlreadyTerminal,
    DiscardStagingAndFailNoWrites,
    RestoreBackupAndFailRolledBack,
    ResumeRollback,
    ContinueVerification,
    ContinueFinalization,
    CompleteRollback,
    MarkFailedRolledBack,
    RecoveryRequired,
}

/// Classifies one persisted Operation conservatively without mutating any path.
///
/// Unknown, malformed, unowned, or contradictory versions always require reviewed recovery.
///
/// # Errors
///
/// Returns an error only when authorized roots or ordinary filesystem inspection are unavailable;
/// contradictory but readable evidence returns [`StartupDecision::RecoveryRequired`].
#[allow(clippy::too_many_lines)]
pub fn classify_startup(
    stored: &StoredOperation,
    roots: &TargetRoots,
) -> Result<StartupDecision, OperationError> {
    if stored.journal.state.is_terminal() {
        return Ok(StartupDecision::AlreadyTerminal);
    }
    if stored.steps.len() != stored.plan.content.steps.len() || !phase_shape_matches_state(stored) {
        return Ok(StartupDecision::RecoveryRequired);
    }

    let mut shapes = Vec::with_capacity(stored.plan.content.steps.len());
    for (plan_step, evidence) in stored.plan.content.steps.iter().zip(&stored.steps) {
        let final_path = match roots.authorize(&plan_step.path) {
            Ok(path) => path.path().to_path_buf(),
            Err(OperationError::StalePlan { .. }) => {
                return Ok(StartupDecision::RecoveryRequired);
            }
            Err(error) => return Err(error),
        };
        let final_absent = match std::fs::symlink_metadata(&final_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Ok(_) => false,
            Err(source) => {
                return Err(OperationError::Filesystem {
                    context: "inspecting final path during startup classification",
                    source,
                });
            }
        };
        let final_before = capture_plan_path(
            roots,
            &plan_step.path,
            &plan_step.before,
            stored.plan.content.bundle_caps,
        )
        .is_ok_and(|actual| fingerprint_matches(&plan_step.before, &actual));
        let final_after = capture_plan_path(
            roots,
            &plan_step.path,
            &plan_step.after,
            stored.plan.content.bundle_caps,
        )
        .is_ok_and(|actual| fingerprint_matches(&plan_step.after, &actual));
        let stage = artifact_state(
            evidence.stage_path.as_deref(),
            &final_path,
            stored.plan.content.operation_id,
            ArtifactKind::Stage,
            &plan_step.after,
            stored.plan.content.bundle_caps,
        );
        let backup = artifact_state(
            evidence.backup_path.as_deref(),
            &final_path,
            stored.plan.content.operation_id,
            ArtifactKind::Backup,
            &plan_step.before,
            stored.plan.content.bundle_caps,
        );
        let rollback = artifact_state(
            evidence.rollback_path.as_deref(),
            &final_path,
            stored.plan.content.operation_id,
            ArtifactKind::Rollback,
            evidence
                .rollback_source
                .as_ref()
                .unwrap_or(&plan_step.after),
            stored.plan.content.bundle_caps,
        );
        shapes.push(classify_step(
            plan_step.action,
            final_before,
            final_after,
            stage,
            backup,
            rollback,
            evidence.commit.status == PhaseStatus::ObservedComplete,
            final_absent,
        ));
    }

    Ok(decision_for(stored.journal.state, &shapes))
}

#[allow(clippy::too_many_lines)]
fn decision_for(state: OperationState, shapes: &[StepShape]) -> StartupDecision {
    if shapes.contains(&StepShape::Unknown) {
        return StartupDecision::RecoveryRequired;
    }
    let all = |allowed: &[StepShape]| shapes.iter().all(|shape| allowed.contains(shape));
    let any = |wanted: StepShape| shapes.contains(&wanted);
    match state {
        OperationState::Planned
        | OperationState::Preflighted
        | OperationState::Snapshotted
        | OperationState::Staged => {
            if all(&[
                StepShape::Untouched,
                StepShape::Uncommitted,
                StepShape::Staged,
            ]) {
                StartupDecision::DiscardStagingAndFailNoWrites
            } else {
                StartupDecision::RecoveryRequired
            }
        }
        OperationState::Committing => {
            if all(&[
                StepShape::Untouched,
                StepShape::Uncommitted,
                StepShape::Staged,
            ]) {
                StartupDecision::DiscardStagingAndFailNoWrites
            } else if all(&[StepShape::Untouched, StepShape::Committed]) {
                StartupDecision::ContinueVerification
            } else if any(StepShape::BackupOnly)
                && !any(StepShape::Committed)
                && all(&[
                    StepShape::Untouched,
                    StepShape::Uncommitted,
                    StepShape::Staged,
                    StepShape::BackupOnly,
                ])
            {
                StartupDecision::RestoreBackupAndFailRolledBack
            } else if all(&[
                StepShape::Untouched,
                StepShape::Uncommitted,
                StepShape::Staged,
                StepShape::BackupOnly,
                StepShape::Committed,
            ]) {
                StartupDecision::ResumeRollback
            } else {
                StartupDecision::RecoveryRequired
            }
        }
        OperationState::Verifying => {
            if all(&[StepShape::Untouched, StepShape::Committed]) {
                StartupDecision::ContinueVerification
            } else if all(&[
                StepShape::Untouched,
                StepShape::Uncommitted,
                StepShape::Staged,
                StepShape::BackupOnly,
                StepShape::Committed,
            ]) {
                StartupDecision::ResumeRollback
            } else {
                StartupDecision::RecoveryRequired
            }
        }
        OperationState::Committed => {
            if all(&[StepShape::Untouched, StepShape::Committed]) {
                StartupDecision::ContinueFinalization
            } else {
                StartupDecision::RecoveryRequired
            }
        }
        OperationState::RollingBack => {
            if all(&[
                StepShape::Untouched,
                StepShape::Uncommitted,
                StepShape::Staged,
                StepShape::RolledBack,
            ]) {
                StartupDecision::CompleteRollback
            } else if all(&[
                StepShape::Untouched,
                StepShape::Uncommitted,
                StepShape::Staged,
                StepShape::BackupOnly,
                StepShape::Committed,
                StepShape::RollbackAside,
                StepShape::RolledBack,
            ]) {
                StartupDecision::ResumeRollback
            } else {
                StartupDecision::RecoveryRequired
            }
        }
        OperationState::RolledBack => {
            if all(&[
                StepShape::Untouched,
                StepShape::Uncommitted,
                StepShape::Staged,
                StepShape::RolledBack,
            ]) {
                StartupDecision::MarkFailedRolledBack
            } else {
                StartupDecision::RecoveryRequired
            }
        }
        OperationState::Finalized | OperationState::Failed | OperationState::RecoveryRequired => {
            StartupDecision::AlreadyTerminal
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactState {
    Absent,
    Matches,
    OwnedMismatch,
    Unknown,
}

fn artifact_state(
    recorded: Option<&str>,
    final_path: &Path,
    operation_id: crate::domain::OperationId,
    kind: ArtifactKind,
    expected: &super::PathFingerprint,
    caps: crate::filesystem::BundleCaps,
) -> ArtifactState {
    let Some(recorded) = recorded else {
        return ArtifactState::Absent;
    };
    let path = PathBuf::from(recorded);
    if path.parent() != final_path.parent() || !artifact_is_owned(&path, operation_id, kind) {
        return ArtifactState::Unknown;
    }
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ArtifactState::Absent,
        Err(_) => ArtifactState::Unknown,
        Ok(_) => {
            capture_raw_path(&path, expected, caps).map_or(ArtifactState::OwnedMismatch, |actual| {
                if fingerprint_matches(expected, &actual) {
                    ArtifactState::Matches
                } else {
                    ArtifactState::OwnedMismatch
                }
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepShape {
    Untouched,
    Uncommitted,
    Staged,
    BackupOnly,
    Committed,
    RollbackAside,
    RolledBack,
    Unknown,
}

#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
fn classify_step(
    action: PlanAction,
    final_before: bool,
    final_after: bool,
    stage: ArtifactState,
    backup: ArtifactState,
    rollback: ArtifactState,
    commit_observed: bool,
    final_absent: bool,
) -> StepShape {
    if stage == ArtifactState::Unknown
        || backup == ArtifactState::Unknown
        || rollback == ArtifactState::Unknown
    {
        return StepShape::Unknown;
    }
    match action {
        PlanAction::LeaveUntouched => {
            if final_before
                && stage == ArtifactState::Absent
                && backup == ArtifactState::Absent
                && rollback == ArtifactState::Absent
            {
                StepShape::Untouched
            } else {
                StepShape::Unknown
            }
        }
        PlanAction::Create => {
            if final_before && backup == ArtifactState::Absent && rollback == ArtifactState::Absent
            {
                if stage == ArtifactState::Matches {
                    StepShape::Staged
                } else if matches!(stage, ArtifactState::Absent | ArtifactState::OwnedMismatch) {
                    StepShape::Uncommitted
                } else {
                    StepShape::Unknown
                }
            } else if final_after
                && stage == ArtifactState::Absent
                && backup == ArtifactState::Absent
                && rollback == ArtifactState::Absent
            {
                StepShape::Committed
            } else if final_before
                && stage == ArtifactState::Absent
                && backup == ArtifactState::Absent
                && rollback == ArtifactState::Matches
            {
                StepShape::RolledBack
            } else {
                StepShape::Unknown
            }
        }
        PlanAction::Replace => {
            if final_before && backup == ArtifactState::Absent && rollback == ArtifactState::Absent
            {
                if stage == ArtifactState::Matches {
                    StepShape::Staged
                } else if matches!(stage, ArtifactState::Absent | ArtifactState::OwnedMismatch) {
                    StepShape::Uncommitted
                } else {
                    StepShape::Unknown
                }
            } else if final_absent
                && !final_before
                && !final_after
                && stage == ArtifactState::Matches
                && backup == ArtifactState::Matches
                && rollback == ArtifactState::Absent
            {
                StepShape::BackupOnly
            } else if final_after
                && stage == ArtifactState::Absent
                && backup == ArtifactState::Matches
                && rollback == ArtifactState::Absent
            {
                StepShape::Committed
            } else if final_before
                && stage == ArtifactState::Absent
                && backup == ArtifactState::Absent
                && rollback == ArtifactState::Matches
            {
                StepShape::RolledBack
            } else if final_absent
                && stage == ArtifactState::Absent
                && backup == ArtifactState::Matches
                && rollback == ArtifactState::Matches
            {
                StepShape::RollbackAside
            } else {
                StepShape::Unknown
            }
        }
        PlanAction::Remove => {
            if final_before && backup == ArtifactState::Absent && rollback == ArtifactState::Absent
            {
                StepShape::Uncommitted
            } else if final_after
                && backup == ArtifactState::Matches
                && rollback == ArtifactState::Absent
            {
                if commit_observed {
                    StepShape::Committed
                } else {
                    StepShape::BackupOnly
                }
            } else if final_before
                && backup == ArtifactState::Absent
                && rollback == ArtifactState::Matches
            {
                StepShape::RolledBack
            } else {
                StepShape::Unknown
            }
        }
    }
}

fn phase_shape_matches_state(stored: &StoredOperation) -> bool {
    stored
        .plan
        .content
        .steps
        .iter()
        .zip(&stored.steps)
        .all(|(plan_step, step)| match stored.journal.state {
            OperationState::Planned | OperationState::Preflighted => {
                step.stage.status == PhaseStatus::NotStarted
                    && step.commit.status == PhaseStatus::NotStarted
                    && step.verify.status == PhaseStatus::NotStarted
                    && step.rollback.status == PhaseStatus::NotStarted
            }
            OperationState::Snapshotted => {
                stage_may_be_in_progress(plan_step.action, step.stage.status)
                    && step.commit.status == PhaseStatus::NotStarted
                    && step.verify.status == PhaseStatus::NotStarted
                    && step.rollback.status == PhaseStatus::NotStarted
            }
            OperationState::Staged => {
                stage_is_complete(plan_step.action, step.stage.status)
                    && step.commit.status == PhaseStatus::NotStarted
                    && step.verify.status == PhaseStatus::NotStarted
                    && step.rollback.status == PhaseStatus::NotStarted
            }
            OperationState::Committing => {
                stage_is_complete(plan_step.action, step.stage.status)
                    && commit_may_be_in_progress(plan_step.action, step.commit.status)
                    && step.verify.status == PhaseStatus::NotStarted
                    && step.rollback.status == PhaseStatus::NotStarted
            }
            OperationState::Verifying => {
                stage_is_complete(plan_step.action, step.stage.status)
                    && commit_is_complete(plan_step.action, step.commit.status)
                    && verify_may_be_in_progress(plan_step.action, step.verify.status)
                    && step.rollback.status == PhaseStatus::NotStarted
            }
            OperationState::Committed => {
                stage_is_complete(plan_step.action, step.stage.status)
                    && commit_is_complete(plan_step.action, step.commit.status)
                    && verify_is_complete(plan_step.action, step.verify.status)
                    && step.rollback.status == PhaseStatus::NotStarted
            }
            OperationState::RollingBack
            | OperationState::RolledBack
            | OperationState::Finalized
            | OperationState::Failed
            | OperationState::RecoveryRequired => true,
        })
}

fn stage_may_be_in_progress(action: PlanAction, status: PhaseStatus) -> bool {
    match action {
        PlanAction::Create | PlanAction::Replace => matches!(
            status,
            PhaseStatus::NotStarted | PhaseStatus::IntentPersisted | PhaseStatus::ObservedComplete
        ),
        PlanAction::Remove | PlanAction::LeaveUntouched => {
            matches!(status, PhaseStatus::NotStarted | PhaseStatus::NotRequired)
        }
    }
}

fn stage_is_complete(action: PlanAction, status: PhaseStatus) -> bool {
    match action {
        PlanAction::Create | PlanAction::Replace => status == PhaseStatus::ObservedComplete,
        PlanAction::Remove | PlanAction::LeaveUntouched => status == PhaseStatus::NotRequired,
    }
}

fn commit_may_be_in_progress(action: PlanAction, status: PhaseStatus) -> bool {
    match action {
        PlanAction::Create | PlanAction::Replace | PlanAction::Remove => matches!(
            status,
            PhaseStatus::NotStarted | PhaseStatus::IntentPersisted | PhaseStatus::ObservedComplete
        ),
        PlanAction::LeaveUntouched => {
            matches!(status, PhaseStatus::NotStarted | PhaseStatus::NotRequired)
        }
    }
}

fn commit_is_complete(action: PlanAction, status: PhaseStatus) -> bool {
    match action {
        PlanAction::Create | PlanAction::Replace | PlanAction::Remove => {
            status == PhaseStatus::ObservedComplete
        }
        PlanAction::LeaveUntouched => status == PhaseStatus::NotRequired,
    }
}

fn verify_may_be_in_progress(action: PlanAction, status: PhaseStatus) -> bool {
    match action {
        PlanAction::Create | PlanAction::Replace | PlanAction::Remove => matches!(
            status,
            PhaseStatus::NotStarted | PhaseStatus::IntentPersisted | PhaseStatus::ObservedComplete
        ),
        PlanAction::LeaveUntouched => {
            matches!(status, PhaseStatus::NotStarted | PhaseStatus::NotRequired)
        }
    }
}

fn verify_is_complete(action: PlanAction, status: PhaseStatus) -> bool {
    match action {
        PlanAction::Create | PlanAction::Replace | PlanAction::Remove => {
            status == PhaseStatus::ObservedComplete
        }
        PlanAction::LeaveUntouched => status == PhaseStatus::NotRequired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_nonterminal_journal_state_has_a_deterministic_safe_action() {
        let cases = [
            (
                OperationState::Planned,
                vec![StepShape::Uncommitted],
                StartupDecision::DiscardStagingAndFailNoWrites,
            ),
            (
                OperationState::Preflighted,
                vec![StepShape::Uncommitted],
                StartupDecision::DiscardStagingAndFailNoWrites,
            ),
            (
                OperationState::Snapshotted,
                vec![StepShape::Staged],
                StartupDecision::DiscardStagingAndFailNoWrites,
            ),
            (
                OperationState::Staged,
                vec![StepShape::Staged],
                StartupDecision::DiscardStagingAndFailNoWrites,
            ),
            (
                OperationState::Committing,
                vec![StepShape::BackupOnly],
                StartupDecision::RestoreBackupAndFailRolledBack,
            ),
            (
                OperationState::Committing,
                vec![
                    StepShape::Untouched,
                    StepShape::Uncommitted,
                    StepShape::Staged,
                ],
                StartupDecision::DiscardStagingAndFailNoWrites,
            ),
            (
                OperationState::Committing,
                vec![StepShape::Committed, StepShape::Uncommitted],
                StartupDecision::ResumeRollback,
            ),
            (
                OperationState::Committing,
                vec![StepShape::Committed],
                StartupDecision::ContinueVerification,
            ),
            (
                OperationState::Verifying,
                vec![StepShape::Committed],
                StartupDecision::ContinueVerification,
            ),
            (
                OperationState::Verifying,
                vec![StepShape::Committed, StepShape::BackupOnly],
                StartupDecision::ResumeRollback,
            ),
            (
                OperationState::Committed,
                vec![StepShape::Committed],
                StartupDecision::ContinueFinalization,
            ),
            (
                OperationState::Committed,
                vec![StepShape::Uncommitted],
                StartupDecision::RecoveryRequired,
            ),
            (
                OperationState::RollingBack,
                vec![StepShape::RolledBack],
                StartupDecision::CompleteRollback,
            ),
            (
                OperationState::RollingBack,
                vec![StepShape::RolledBack, StepShape::Committed],
                StartupDecision::ResumeRollback,
            ),
            (
                OperationState::RollingBack,
                vec![StepShape::RollbackAside, StepShape::Committed],
                StartupDecision::ResumeRollback,
            ),
            (
                OperationState::RolledBack,
                vec![StepShape::RolledBack],
                StartupDecision::MarkFailedRolledBack,
            ),
            (
                OperationState::RolledBack,
                vec![StepShape::Unknown],
                StartupDecision::RecoveryRequired,
            ),
        ];

        for (state, shapes, expected) in cases {
            assert_eq!(decision_for(state, &shapes), expected, "state {state:?}");
        }
    }
}
