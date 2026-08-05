//! IDs, safe values, entities, and state machines.

mod digest;
mod ids;
mod names;
mod state;
mod time;

pub use digest::{BundleDigest, DigestParseError, RevisionId};
pub use ids::{
    ActivityId, DeploymentId, ObservationId, OperationId, ProjectId, ScanRunId, SkillId,
    SnapshotId, TargetId, TrashEntryId, VaultId, WorkspaceRootId,
};
pub use names::{
    AdapterId, BundleRelativePath, DeploymentName, NameError, PathCaseSensitivity,
    normalized_collision_key, normalized_path_identity,
};
pub use state::{
    DeploymentHealth, DeploymentMode, DuplicateClassification, ManagedTargetObservation,
    OperationOutcome, OperationState, OperationTone, OperationTransitionError, Ownership,
    SkillLifecycle, SymlinkTargetObservation, classify_duplicate, managed_copy_health, ownership,
    symlink_health,
};
pub use time::{DurationMillis, MonotonicTimer, TimeError, UtcTimestamp};
