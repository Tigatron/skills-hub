use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::BundleDigest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillLifecycle {
    Active,
    Trashed,
    PermanentlyRemoved,
}

impl SkillLifecycle {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Active, Self::Trashed)
                | (Self::Trashed, Self::Active | Self::PermanentlyRemoved)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    External,
    Vaulted,
    Managed,
}

#[must_use]
pub const fn ownership(
    lifecycle: Option<SkillLifecycle>,
    has_working_version: bool,
    active_deployment_count: usize,
) -> Option<Ownership> {
    match lifecycle {
        None => Some(Ownership::External),
        Some(SkillLifecycle::Active) if has_working_version && active_deployment_count == 0 => {
            Some(Ownership::Vaulted)
        }
        Some(SkillLifecycle::Active) if has_working_version && active_deployment_count > 0 => {
            Some(Ownership::Managed)
        }
        Some(
            SkillLifecycle::Active | SkillLifecycle::Trashed | SkillLifecycle::PermanentlyRemoved,
        ) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateClassification {
    ExactDuplicate,
    NameConflict,
    ProbableDuplicateOrRename,
    Unrelated,
    Unverified,
}

#[must_use]
pub fn classify_duplicate(
    left_name_key: &str,
    left_digest: Option<BundleDigest>,
    right_name_key: &str,
    right_digest: Option<BundleDigest>,
) -> DuplicateClassification {
    let (Some(left_digest), Some(right_digest)) = (left_digest, right_digest) else {
        return DuplicateClassification::Unverified;
    };
    match (left_name_key == right_name_key, left_digest == right_digest) {
        (true, true) => DuplicateClassification::ExactDuplicate,
        (true, false) => DuplicateClassification::NameConflict,
        (false, true) => DuplicateClassification::ProbableDuplicateOrRename,
        (false, false) => DuplicateClassification::Unrelated,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentHealth {
    Clean,
    VaultAhead,
    TargetModified,
    MissingTarget,
    BrokenLink,
    Conflict,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedTargetObservation {
    Missing,
    EntryConflict,
    Unreadable,
    Verified(BundleDigest),
}

#[must_use]
pub fn managed_copy_health(
    expected: BundleDigest,
    vault: Option<BundleDigest>,
    target: ManagedTargetObservation,
) -> DeploymentHealth {
    let target = match target {
        ManagedTargetObservation::Missing => return DeploymentHealth::MissingTarget,
        ManagedTargetObservation::EntryConflict => return DeploymentHealth::Conflict,
        ManagedTargetObservation::Unreadable => return DeploymentHealth::Unverified,
        ManagedTargetObservation::Verified(digest) => digest,
    };
    let Some(vault) = vault else {
        return DeploymentHealth::Unverified;
    };

    if target == expected && vault == expected {
        DeploymentHealth::Clean
    } else if target == expected {
        DeploymentHealth::VaultAhead
    } else if vault == expected {
        DeploymentHealth::TargetModified
    } else if target != vault {
        DeploymentHealth::Conflict
    } else {
        DeploymentHealth::Unverified
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkTargetObservation {
    Missing,
    Broken,
    Conflict,
    Correct,
}

#[must_use]
pub fn symlink_health(
    expected: BundleDigest,
    vault: Option<BundleDigest>,
    target: SymlinkTargetObservation,
) -> DeploymentHealth {
    match target {
        SymlinkTargetObservation::Missing => DeploymentHealth::MissingTarget,
        SymlinkTargetObservation::Broken => DeploymentHealth::BrokenLink,
        SymlinkTargetObservation::Conflict => DeploymentHealth::Conflict,
        SymlinkTargetObservation::Correct => match vault {
            Some(vault) if vault == expected => DeploymentHealth::Clean,
            Some(_) => DeploymentHealth::VaultAhead,
            None => DeploymentHealth::Unverified,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMode {
    Symlink,
    ManagedCopy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Planned,
    Preflighted,
    Snapshotted,
    Staged,
    Committing,
    Verifying,
    Committed,
    Finalized,
    RollingBack,
    RolledBack,
    Failed,
    RecoveryRequired,
}

impl OperationState {
    /// Applies one legal state-machine transition.
    ///
    /// # Errors
    ///
    /// Returns [`OperationTransitionError`] for skipped, reversed, or terminal transitions.
    pub fn transition(self, next: Self) -> Result<Self, OperationTransitionError> {
        if matches!(
            (self, next),
            (Self::Planned, Self::Preflighted)
                | (Self::Preflighted, Self::Snapshotted)
                | (Self::Snapshotted, Self::Staged)
                | (Self::Staged, Self::Committing)
                | (Self::Committing, Self::Verifying)
                | (Self::Verifying, Self::Committed)
                | (Self::Committed, Self::Finalized)
                | (Self::RollingBack, Self::RolledBack | Self::RecoveryRequired)
                | (Self::RolledBack, Self::Failed)
        ) || (!self.is_terminal()
            && matches!(
                next,
                Self::RollingBack | Self::Failed | Self::RecoveryRequired
            ))
        {
            Ok(next)
        } else {
            Err(OperationTransitionError {
                from: self,
                to: next,
            })
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finalized | Self::Failed | Self::RecoveryRequired
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid Operation transition from {from:?} to {to:?}")]
pub struct OperationTransitionError {
    pub from: OperationState,
    pub to: OperationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Succeeded,
    CancelledNoWrites,
    FailedNoWrites,
    FailedRolledBack,
    RecoveryRequired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn digest(byte: u8) -> BundleDigest {
        BundleDigest::from_bytes([byte; 32])
    }

    #[test]
    fn ownership_is_computed_from_durable_facts() {
        assert_eq!(ownership(None, false, 0), Some(Ownership::External));
        assert_eq!(
            ownership(Some(SkillLifecycle::Active), true, 0),
            Some(Ownership::Vaulted)
        );
        assert_eq!(
            ownership(Some(SkillLifecycle::Active), true, 2),
            Some(Ownership::Managed)
        );
        assert_eq!(ownership(Some(SkillLifecycle::Trashed), true, 0), None);
    }

    #[test]
    fn managed_copy_health_matches_the_accepted_truth_table() {
        let expected = digest(1);
        let changed_vault = digest(2);
        let changed_target = digest(3);

        let cases = [
            (
                None,
                ManagedTargetObservation::Missing,
                DeploymentHealth::MissingTarget,
            ),
            (
                Some(expected),
                ManagedTargetObservation::EntryConflict,
                DeploymentHealth::Conflict,
            ),
            (
                Some(expected),
                ManagedTargetObservation::Unreadable,
                DeploymentHealth::Unverified,
            ),
            (
                Some(expected),
                ManagedTargetObservation::Verified(expected),
                DeploymentHealth::Clean,
            ),
            (
                Some(changed_vault),
                ManagedTargetObservation::Verified(expected),
                DeploymentHealth::VaultAhead,
            ),
            (
                Some(expected),
                ManagedTargetObservation::Verified(changed_target),
                DeploymentHealth::TargetModified,
            ),
            (
                Some(changed_vault),
                ManagedTargetObservation::Verified(changed_target),
                DeploymentHealth::Conflict,
            ),
            (
                Some(changed_vault),
                ManagedTargetObservation::Verified(changed_vault),
                DeploymentHealth::Unverified,
            ),
        ];

        for (vault, target, expected_health) in cases {
            assert_eq!(
                managed_copy_health(expected, vault, target),
                expected_health
            );
        }
    }

    #[test]
    fn symlink_health_distinguishes_missing_broken_and_changed_vault() {
        let expected = digest(1);
        assert_eq!(
            symlink_health(expected, Some(expected), SymlinkTargetObservation::Missing),
            DeploymentHealth::MissingTarget
        );
        assert_eq!(
            symlink_health(expected, Some(expected), SymlinkTargetObservation::Broken),
            DeploymentHealth::BrokenLink
        );
        assert_eq!(
            symlink_health(expected, Some(digest(2)), SymlinkTargetObservation::Correct),
            DeploymentHealth::VaultAhead
        );
    }

    #[test]
    fn operation_state_rejects_skips_and_terminal_reentry() {
        assert!(
            OperationState::Planned
                .transition(OperationState::Preflighted)
                .is_ok()
        );
        assert!(
            OperationState::Planned
                .transition(OperationState::Committing)
                .is_err()
        );
        assert!(
            OperationState::Finalized
                .transition(OperationState::RollingBack)
                .is_err()
        );
    }

    proptest! {
        #[test]
        fn duplicate_classification_is_independent_of_enumeration_order(
            left_name in "[a-z]{1,8}",
            right_name in "[a-z]{1,8}",
            left_byte in any::<u8>(),
            right_byte in any::<u8>(),
        ) {
            let forward = classify_duplicate(
                &left_name,
                Some(digest(left_byte)),
                &right_name,
                Some(digest(right_byte)),
            );
            let reverse = classify_duplicate(
                &right_name,
                Some(digest(right_byte)),
                &left_name,
                Some(digest(left_byte)),
            );
            prop_assert_eq!(forward, reverse);
        }
    }
}
