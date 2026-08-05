//! Reviewed plans, execution, journaling, rollback, and startup recovery.

mod executor;
mod journal;
mod plan;
mod planner;
mod recovery;

pub use executor::{
    CancellationToken, NoopOperationEventSink, NoopOperationFailpoints, OperationBoundary,
    OperationCoordinator, OperationError, OperationErrorCode, OperationErrorEnvelope,
    OperationEvent, OperationEventSink, OperationExecution, OperationExecutor, OperationFailpoints,
    OperationFinalizer, OperationHookError, SnapshotRegistrar, SnapshotRegistration,
    StagingProvider, SuggestedAction, TargetRoots,
};
pub use journal::{
    JournalError, OperationFailure, OperationJournal, OperationStore, PhaseEvidence, PhaseStatus,
    SnapshotProtection, StepJournal, StoredOperation,
};

pub use plan::{
    BatchDeploymentAction, BatchDeploymentEntryEvidence, BatchDeploymentInverseEvidence,
    BatchDeploymentPlanContext, CapabilityStatus, DeploymentPlanContext, DeploymentProductAction,
    DeploymentSkillEvidence, DeploymentTargetEvidence, ManagedDeploymentEvidence, OperationKind,
    OperationPlan, OperationPlanContent, OwnershipChoice, OwnershipDecision, PathFingerprint,
    PlanAction, PlanBlocker, PlanBlockerCode, PlanBuildError, PlanDigest, PlanPath, PlanStep,
    RecoverySummary, TakeoverDecision, TakeoverObservationEvidence, TakeoverObservationStatus,
    TakeoverPlanContext, TakeoverReplacementEvidence, TakeoverSkillEvidence, TakeoverTargetScope,
    TargetCapabilityEvidence, UndeployResolution,
};
pub use planner::{OperationIntent, OperationPlanner, PlanBuilder};
pub use recovery::{StartupDecision, classify_startup};
