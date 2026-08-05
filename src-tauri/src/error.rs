//! Serializable application errors and internal error conversion.

use serde::Serialize;
use specta::Type;
use thiserror::Error;

use crate::{
    application::{
        activity::ActivityError, deployment::DeploymentError, scanning::ScanningServiceError,
        takeover::TakeoverError,
    },
    operations::OperationError,
    runtime::{BlockingWorkError, RuntimeStateError},
};

#[derive(Debug, Clone, Copy, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AppErrorCode {
    Internal,
    InvalidInput,
    NotFound,
    UnsafePath,
    UnsupportedBundle,
    NameCollision,
    StalePlan,
    OperationBusy,
    Cancelled,
    IoFailure,
    DatabaseFailure,
    VerificationFailed,
    RolledBack,
    RecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Type, Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct AppErrorView {
    pub code: AppErrorCode,
    pub title: String,
    pub message: String,
    pub retryable: bool,
    pub recovery_action: Option<String>,
}

impl From<BlockingWorkError> for AppErrorView {
    fn from(_error: BlockingWorkError) -> Self {
        Self {
            code: AppErrorCode::Internal,
            title: "Background work stopped".to_owned(),
            message: "Skills Hub could not complete a background task.".to_owned(),
            retryable: true,
            recovery_action: Some("retry".to_owned()),
        }
    }
}

impl From<RuntimeStateError> for AppErrorView {
    fn from(error: RuntimeStateError) -> Self {
        match error {
            RuntimeStateError::HomeUnavailable => Self {
                code: AppErrorCode::Internal,
                title: "Home directory unavailable".to_owned(),
                message: "Skills Hub could not resolve the current macOS home directory."
                    .to_owned(),
                retryable: false,
                recovery_action: None,
            },
            RuntimeStateError::VaultNotInitialized => Self {
                code: AppErrorCode::NotFound,
                title: "Choose a Vault first".to_owned(),
                message: "Scanning needs an initialized Vault to store its read-only index. No agent files were changed."
                    .to_owned(),
                retryable: false,
                recovery_action: Some("initialize_vault".to_owned()),
            },
            RuntimeStateError::StartupRecoveryFailed => app_error(
                AppErrorCode::RecoveryRequired, "Startup recovery failed",
                "Skills Hub could not establish authoritative operation state.", false,
                Some("inspect_recovery"),
            ),
        }
    }
}

impl From<ActivityError> for AppErrorView {
    fn from(error: ActivityError) -> Self {
        match error {
            ActivityError::InvalidLimit | ActivityError::InvalidId => app_error(
                AppErrorCode::InvalidInput,
                "Invalid Activity request",
                error.to_string(),
                false,
                None,
            ),
            ActivityError::NotFound => app_error(
                AppErrorCode::NotFound,
                "Activity not found",
                error.to_string(),
                false,
                None,
            ),
            _ => app_error(
                AppErrorCode::DatabaseFailure,
                "Activity evidence unavailable",
                error.to_string(),
                true,
                Some("retry"),
            ),
        }
    }
}

impl From<ScanningServiceError> for AppErrorView {
    fn from(error: ScanningServiceError) -> Self {
        match error {
            ScanningServiceError::JobNotFound => Self {
                code: AppErrorCode::NotFound,
                title: "Scan not found".to_owned(),
                message: "That scan job is not available in this application session.".to_owned(),
                retryable: false,
                recovery_action: Some("start_scan".to_owned()),
            },
            ScanningServiceError::InvalidLibraryLimit { maximum } => Self {
                code: AppErrorCode::InvalidInput,
                title: "Invalid Library page size".to_owned(),
                message: format!("Choose between 1 and {maximum} items per page."),
                retryable: false,
                recovery_action: None,
            },
            ScanningServiceError::UnsupportedPath => Self {
                code: AppErrorCode::UnsafePath,
                title: "Unsupported filesystem path".to_owned(),
                message: "The configured path cannot be represented safely. No files were changed."
                    .to_owned(),
                retryable: false,
                recovery_action: Some("review_path".to_owned()),
            },
            ScanningServiceError::Repository(_) => Self {
                code: AppErrorCode::DatabaseFailure,
                title: "Scan index unavailable".to_owned(),
                message: "Skills Hub could not update its local scan index. No agent files were changed."
                    .to_owned(),
                retryable: true,
                recovery_action: Some("retry".to_owned()),
            },
            ScanningServiceError::Blocking(_) | ScanningServiceError::StatePoisoned => Self {
                code: AppErrorCode::Internal,
                title: "Background scan stopped".to_owned(),
                message: "Skills Hub could not complete the background scan. No agent files were changed."
                    .to_owned(),
                retryable: true,
                recovery_action: Some("retry".to_owned()),
            },
        }
    }
}

impl From<TakeoverError> for AppErrorView {
    fn from(error: TakeoverError) -> Self {
        match error {
            TakeoverError::InvalidId { .. }
            | TakeoverError::InvalidSelection(_)
            | TakeoverError::InvalidPreviewPath => app_error(
                AppErrorCode::InvalidInput,
                "Invalid request",
                error.to_string(),
                false,
                None,
            ),
            TakeoverError::ObservationMissing | TakeoverError::SkillMissing => app_error(
                AppErrorCode::NotFound,
                "Item not found",
                error.to_string(),
                false,
                Some("refresh_library"),
            ),
            TakeoverError::ObservationNotExternal => app_error(
                AppErrorCode::StalePlan,
                "Source changed",
                "The selected external Skill no longer matches the reviewed observation. No active paths were changed.",
                false,
                Some("review_new_plan"),
            ),
            TakeoverError::UnsafePreviewPath => app_error(
                AppErrorCode::UnsafePath,
                "Unsafe preview path",
                error.to_string(),
                false,
                None,
            ),
            TakeoverError::PreviewTooLarge | TakeoverError::PreviewNotUtf8 => app_error(
                AppErrorCode::UnsupportedBundle,
                "Preview unavailable",
                error.to_string(),
                false,
                None,
            ),
            TakeoverError::UnstablePreview => app_error(
                AppErrorCode::VerificationFailed,
                "Content changed during verification",
                error.to_string(),
                true,
                Some("retry"),
            ),
            TakeoverError::Io(_) => app_error(
                AppErrorCode::IoFailure,
                "Filesystem operation failed",
                "Skills Hub could not safely complete the local filesystem operation.",
                true,
                Some("retry"),
            ),
            TakeoverError::Persistence(_) => app_error(
                AppErrorCode::DatabaseFailure,
                "Vault index unavailable",
                "Skills Hub could not update the local Vault index.",
                true,
                Some("retry"),
            ),
            TakeoverError::Operation(operation) => operation_error(&operation),
            TakeoverError::Journal(_) => app_error(
                AppErrorCode::RecoveryRequired,
                "Operation evidence unavailable",
                "Skills Hub could not verify the durable Operation evidence. Existing versions were preserved.",
                false,
                Some("inspect_recovery"),
            ),
        }
    }
}

impl From<DeploymentError> for AppErrorView {
    fn from(error: DeploymentError) -> Self {
        match error {
            DeploymentError::InvalidId { .. } => app_error(
                AppErrorCode::InvalidInput,
                "Invalid request",
                error.to_string(),
                false,
                None,
            ),
            DeploymentError::InvalidTargetDirectory => app_error(
                AppErrorCode::UnsafePath,
                "Unsafe Target directory",
                "The selected Target is unavailable, changed, or overlaps the Vault. No managed path was changed.",
                false,
                Some("select_target"),
            ),
            DeploymentError::SkillMissing
            | DeploymentError::TargetMissing
            | DeploymentError::DeploymentMissing => app_error(
                AppErrorCode::NotFound,
                "Deployment item not found",
                error.to_string(),
                false,
                Some("refresh_deployments"),
            ),
            DeploymentError::UnmanagedCollision => app_error(
                AppErrorCode::NameCollision,
                "Target name is occupied",
                "An unmanaged or differently owned entry occupies this exact Target name. No files were changed.",
                false,
                Some("review_collision"),
            ),
            DeploymentError::CapabilityBlocked(_) => app_error(
                AppErrorCode::UnsupportedBundle,
                "Target capability is not supported",
                error.to_string(),
                false,
                Some("review_new_plan"),
            ),
            DeploymentError::DriftBlocked(_) => app_error(
                AppErrorCode::StalePlan,
                "Deployment changed",
                error.to_string(),
                false,
                Some("review_drift"),
            ),
            DeploymentError::PlanningCancelled => app_error(
                AppErrorCode::Cancelled,
                "Planning cancelled",
                "No Operation was persisted and no managed path was changed.",
                false,
                None,
            ),
            DeploymentError::Io(_) | DeploymentError::Manifest(_) => app_error(
                AppErrorCode::IoFailure,
                "Deployment filesystem evidence unavailable",
                "Skills Hub could not safely verify or publish deployment evidence.",
                true,
                Some("retry"),
            ),
            DeploymentError::Persistence(_) => app_error(
                AppErrorCode::DatabaseFailure,
                "Deployment index unavailable",
                "Skills Hub could not update its local deployment index.",
                true,
                Some("retry"),
            ),
            DeploymentError::Operation(operation) => operation_error(&operation),
            DeploymentError::Journal(_) => app_error(
                AppErrorCode::RecoveryRequired,
                "Operation evidence unavailable",
                "Skills Hub could not verify durable deployment evidence. Existing versions were preserved.",
                false,
                Some("inspect_recovery"),
            ),
        }
    }
}

fn operation_error(error: &OperationError) -> AppErrorView {
    let (code, title, retryable, action) = match error {
        OperationError::MutationBusy => (
            AppErrorCode::OperationBusy,
            "Another operation is running",
            true,
            Some("wait"),
        ),
        OperationError::Cancelled => (AppErrorCode::Cancelled, "Operation cancelled", false, None),
        OperationError::PlanExpired
        | OperationError::PlanDigestMismatch
        | OperationError::StalePlan { .. } => (
            AppErrorCode::StalePlan,
            "Plan needs review again",
            false,
            Some("review_new_plan"),
        ),
        OperationError::ExecutionFailedRolledBack(_) => (
            AppErrorCode::RolledBack,
            "Operation rolled back",
            false,
            Some("review_new_plan"),
        ),
        OperationError::RecoveryRequired(_)
        | OperationError::RollbackMismatch(_)
        | OperationError::RecoveryPending(_)
        | OperationError::FinalizationInterrupted(_)
        | OperationError::CleanupContainment => (
            AppErrorCode::RecoveryRequired,
            "Recovery required",
            false,
            Some("inspect_recovery"),
        ),
        OperationError::VerifyFailed(_) => (
            AppErrorCode::VerificationFailed,
            "Verification failed",
            false,
            Some("inspect_recovery"),
        ),
        OperationError::Filesystem { .. } => (
            AppErrorCode::IoFailure,
            "Filesystem operation failed",
            true,
            Some("retry"),
        ),
        OperationError::CoordinatorUnavailable
        | OperationError::UnknownTarget(_)
        | OperationError::InvalidPlan(_)
        | OperationError::PlanBlocked(_)
        | OperationError::SnapshotFailed(_)
        | OperationError::StageFailed(_)
        | OperationError::CommitFailed(_)
        | OperationError::FingerprintFailed(_)
        | OperationError::ArtifactCollision
        | OperationError::Journal(_)
        | OperationError::InjectedFailure(_) => (
            AppErrorCode::VerificationFailed,
            "Operation stopped safely",
            true,
            Some("review_operation"),
        ),
    };
    let message = error.envelope().summary;
    app_error(code, title, message, retryable, action)
}

fn app_error(
    code: AppErrorCode,
    title: &str,
    message: impl Into<String>,
    retryable: bool,
    recovery_action: Option<&str>,
) -> AppErrorView {
    AppErrorView {
        code,
        title: title.to_owned(),
        message: message.into(),
        retryable,
        recovery_action: recovery_action.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_view_serializes_as_stable_camel_case_contract() {
        let error = AppErrorView {
            code: AppErrorCode::Internal,
            title: "Title".to_owned(),
            message: "Message".to_owned(),
            retryable: true,
            recovery_action: Some("retry".to_owned()),
        };

        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], "internal");
        assert_eq!(value["recoveryAction"], "retry");
    }
}
