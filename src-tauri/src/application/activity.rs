use std::str::FromStr;

use serde::{Deserialize, Serialize};
use specta::Type;
use thiserror::Error;

use crate::{
    domain::{ActivityId, BundleRelativePath, OperationOutcome},
    operations::{OperationKind, OperationPlan, OperationStore, StoredOperation},
    persistence::{
        ActivityQuery as RepositoryQuery, ActivityRecord, OperationRecord, Repositories,
        RepositoryError,
    },
};

const MAXIMUM_PAGE_SIZE: u16 = 200;

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActivityQuery {
    pub kind: Option<String>,
    pub outcome: Option<String>,
    pub limit: u16,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActivityItem {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub outcome: Option<String>,
    pub summary: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub operation_id: Option<String>,
    pub scan_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActivityDetail {
    pub item: ActivityItem,
    pub details_json: String,
    pub operation: Option<ActivityOperationEvidence>,
    pub scan: Option<ActivityScanEvidence>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActivityOperationEvidence {
    pub recovery_available: bool,
    pub error_code: Option<String>,
    pub failed_step: Option<u32>,
    pub plan_reference: String,
    pub journal_reference: String,
    pub recovery_references: Vec<String>,
    pub paths: Vec<ActivityPathEvidence>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActivityPathEvidence {
    pub step_order: u32,
    pub path: String,
    pub requested_mode: Option<String>,
    pub resolved_mode: Option<String>,
    pub stage_path: Option<String>,
    pub backup_path: Option<String>,
    pub rollback_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ActivityScanEvidence {
    pub diagnostic_count: u32,
    pub error_codes: Vec<String>,
}

#[derive(Clone)]
pub struct ActivityService {
    repositories: Repositories,
    store: OperationStore,
}

impl ActivityService {
    #[must_use]
    pub const fn new(repositories: Repositories, store: OperationStore) -> Self {
        Self {
            repositories,
            store,
        }
    }

    pub fn list(&self, query: ActivityQuery) -> Result<Vec<ActivityItem>, ActivityError> {
        if query.limit == 0 || query.limit > MAXIMUM_PAGE_SIZE {
            return Err(ActivityError::InvalidLimit);
        }
        self.repositories
            .activity_list(RepositoryQuery {
                kind: query.kind,
                outcome: query.outcome,
                limit: query.limit,
            })?
            .into_iter()
            .map(|record| {
                Ok(ActivityItem {
                    id: record.id.to_string(),
                    kind: record.kind,
                    state: record.state,
                    outcome: record.outcome,
                    summary: record.summary,
                    started_at: record.started_at.to_string(),
                    completed_at: record.completed_at.map(|value| value.to_string()),
                    operation_id: record.operation_id.map(|value| value.to_string()),
                    scan_run_id: record.scan_run_id.map(|value| value.to_string()),
                })
            })
            .collect()
    }

    pub fn detail(&self, id: &str) -> Result<ActivityDetail, ActivityError> {
        let id = ActivityId::from_str(id).map_err(|_| ActivityError::InvalidId)?;
        let detail = self
            .repositories
            .activity_detail(id)?
            .ok_or(ActivityError::NotFound)?;
        let item = ActivityItem {
            id: detail.item.id.to_string(),
            kind: detail.item.kind,
            state: detail.item.state,
            outcome: detail.item.outcome,
            summary: detail.item.summary,
            started_at: detail.item.started_at.to_string(),
            completed_at: detail.item.completed_at.map(|value| value.to_string()),
            operation_id: detail.item.operation_id.map(|value| value.to_string()),
            scan_run_id: detail.item.scan_run_id.map(|value| value.to_string()),
        };
        let operation = detail
            .item
            .operation_id
            .map(|operation_id| self.store.load(operation_id))
            .transpose()?
            .map(|stored| {
                let operation_root = format!(".manager/operations/{}", stored.journal.operation_id);
                ActivityOperationEvidence {
                    recovery_available: stored.journal.outcome
                        == Some(OperationOutcome::RecoveryRequired)
                        || !stored.journal.snapshot_protections.is_empty(),
                    error_code: stored
                        .journal
                        .failure
                        .as_ref()
                        .map(|failure| failure.code.clone()),
                    failed_step: stored
                        .journal
                        .failure
                        .as_ref()
                        .and_then(|failure| failure.failed_step),
                    plan_reference: format!("{operation_root}/plan.json"),
                    journal_reference: format!("{operation_root}/journal.json"),
                    recovery_references: stored
                        .journal
                        .snapshot_protections
                        .iter()
                        .map(|protection| protection.reference.clone())
                        .collect(),
                    paths: stored
                        .plan
                        .content
                        .steps
                        .iter()
                        .zip(&stored.steps)
                        .map(|(plan, actual)| ActivityPathEvidence {
                            step_order: plan.order,
                            path: plan.path.display_path().to_owned(),
                            requested_mode: plan
                                .requested_mode
                                .map(|mode| format!("{mode:?}").to_lowercase()),
                            resolved_mode: plan
                                .resolved_mode
                                .map(|mode| format!("{mode:?}").to_lowercase()),
                            stage_path: actual.stage_path.clone(),
                            backup_path: actual.backup_path.clone(),
                            rollback_path: actual.rollback_path.clone(),
                        })
                        .collect(),
                }
            });
        let scan = detail.item.scan_run_id.map(|_| ActivityScanEvidence {
            diagnostic_count: u32::try_from(
                detail.details["diagnosticCount"].as_u64().unwrap_or(0),
            )
            .unwrap_or(u32::MAX),
            error_codes: detail.details["errorCodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect(),
        });
        Ok(ActivityDetail {
            item,
            details_json: serde_json::to_string(&detail.details)?,
            operation,
            scan,
        })
    }

    /// Rebuilds only terminal, fully validated journal evidence. Nonterminal Operations are ignored.
    pub fn rebuild_terminal_operations(&self) -> Result<usize, ActivityError> {
        let mut projected = 0;
        for operation_id in self.store.operation_ids()? {
            projected += usize::from(self.project_terminal_operation(operation_id)?);
        }
        Ok(projected)
    }

    /// Projects one terminal Operation from its durable journal. Repeating it is idempotent.
    pub fn project_terminal_operation(
        &self,
        operation_id: crate::domain::OperationId,
    ) -> Result<bool, ActivityError> {
        let stored = self.store.load(operation_id)?;
        if !stored.journal.state.is_terminal() {
            return Ok(false);
        }
        let Some(activity_id) = plan_activity_id(&stored.plan) else {
            return Ok(false);
        };
        let operation = OperationRecord {
            id: stored.plan.content.operation_id,
            plan_digest: stored.plan.plan_digest.to_string(),
            operation_type: operation_kind_text(stored.plan.content.kind).to_owned(),
            state: stored.journal.state,
            outcome: stored.journal.outcome,
            recovery_state: (stored.journal.outcome == Some(OperationOutcome::RecoveryRequired))
                .then(|| "recovery_required".to_owned()),
            journal_path: BundleRelativePath::parse(&format!(
                ".manager/operations/{}/journal.json",
                stored.plan.content.operation_id
            ))
            .map_err(|_| ActivityError::InvalidTerminalEvidence)?,
            created_at: stored.journal.created_at,
            updated_at: stored.journal.updated_at,
            finalized_at: stored.journal.finalized_at,
        };
        self.repositories
            .finalize_operation(operation, operation_activity(activity_id, &stored)?)?;
        Ok(true)
    }
}

const fn operation_kind_text(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::TakeOver => "takeover",
        OperationKind::Deploy => "deploy",
        OperationKind::Undeploy => "undeploy",
        OperationKind::MoveToTrash => "move_to_trash",
        OperationKind::Restore => "restore",
        OperationKind::PermanentlyDelete => "permanently_delete",
        OperationKind::Undo => "undo",
    }
}

// Product schemas seal the Activity identity. Future plan contexts extend only this helper.
fn plan_activity_id(plan: &OperationPlan) -> Option<ActivityId> {
    plan.content
        .takeover
        .as_ref()
        .map(|context| context.skill.activity_id)
        .or_else(|| {
            plan.content
                .deployment
                .as_ref()
                .map(|context| context.activity_id)
        })
        .or_else(|| {
            plan.content
                .batch_deployment
                .as_ref()
                .map(|context| context.activity_id)
        })
}

fn operation_activity(
    id: ActivityId,
    stored: &StoredOperation,
) -> Result<ActivityRecord, ActivityError> {
    let journal = &stored.journal;
    let outcome = journal
        .outcome
        .ok_or(ActivityError::InvalidTerminalEvidence)?;
    let kind = if stored.plan.content.takeover.is_some() {
        "takeover"
    } else if stored.plan.content.deployment.is_some()
        || stored.plan.content.batch_deployment.is_some()
    {
        "deployment"
    } else {
        "operation"
    };
    let details = serde_json::json!({
        "planReference": format!(".manager/operations/{}/plan.json", journal.operation_id),
        "journalReference": format!(".manager/operations/{}/journal.json", journal.operation_id),
        "recoveryReference": (outcome == OperationOutcome::RecoveryRequired).then(|| format!(".manager/operations/{}/journal.json", journal.operation_id)),
        "recoveryAvailable": outcome == OperationOutcome::RecoveryRequired || !journal.snapshot_protections.is_empty(),
        "errorCode": journal.failure.as_ref().map(|failure| &failure.code),
        "failedStep": journal.failure.as_ref().and_then(|failure| failure.failed_step),
        "actualSteps": stored.steps,
        "planContext": serde_json::to_value(&stored.plan.content)?,
    });
    Ok(ActivityRecord {
        id,
        operation_id: Some(journal.operation_id),
        kind: kind.to_owned(),
        state: format!("{:?}", journal.state).to_lowercase(),
        outcome: Some(outcome),
        summary: format!("{kind} finished: {outcome:?}"),
        details,
        started_at: journal.created_at,
        completed_at: journal.finalized_at,
    })
}

#[derive(Debug, Error)]
pub enum ActivityError {
    #[error("activity page size must be between 1 and 200")]
    InvalidLimit,
    #[error("invalid activity id")]
    InvalidId,
    #[error("activity not found")]
    NotFound,
    #[error("terminal journal has no outcome")]
    InvalidTerminalEvidence,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error(transparent)]
    Journal(#[from] crate::operations::JournalError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
