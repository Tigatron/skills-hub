import { useMutation } from '@tanstack/react-query';
import type { RefObject } from 'react';

import type { AnyOperationView } from '../bindings';
import { api, operationOutcomeLabel, type ReviewedPlan } from '../lib/api';
import {
  DangerButton,
  ErrorBanner,
  MetaRow,
  PathText,
  PrimaryButton,
  SecondaryButton,
  StatusPill,
} from './components';
import styles from './thin.module.css';

export function OperationPanel({
  plan,
  operation,
  busy,
  onExecute,
  onCancel,
  onClear,
  focusReturnRef,
}: {
  plan: ReviewedPlan | null;
  operation: AnyOperationView | null;
  busy: boolean;
  onExecute: () => void;
  onCancel: () => void;
  onClear: () => void;
  /** Element that should regain focus after dismiss/cancel/completion. */
  focusReturnRef?: RefObject<HTMLElement | null>;
}) {
  const exportPlan = useMutation({
    mutationFn: (operationId: string) => api.operationPlanExport({ operationId }),
  });

  const restoreFocus = () => {
    const target = focusReturnRef?.current;
    if (target && typeof target.focus === 'function') {
      target.focus();
    }
  };

  if (!plan && !operation) {
    return (
      <div className={styles.emptyState}>
        <h3>No plan under review</h3>
        <p>
          Choose a Library or Deployments action to generate a plan. Paths, modes, and recovery stay
          visible while execution runs. Ownership is never updated optimistically.
        </p>
      </div>
    );
  }

  const tone = operation?.value.tone ?? 'neutral';
  const terminal = operation?.value.terminal ?? false;
  const currentExport =
    plan &&
    exportPlan.data?.operationId === plan.plan.operationId &&
    exportPlan.data.planDigest === plan.plan.planDigest
      ? exportPlan.data
      : null;

  return (
    <div className={styles.stack}>
      {operation ? (
        <div className={styles.operationBanner} data-tone={tone} role="status">
          <div className={styles.listItemTitle}>
            <strong>Operation {operation.value.operationId}</strong>
            <StatusPill tone={tone}>{operationOutcomeLabel(operation)}</StatusPill>
          </div>
          <p className={styles.muted}>
            State {operation.value.state}
            {operation.value.failure ? ` · ${operation.value.failure}` : ''}
            {operation.value.replayed ? ' · replayed' : ''}
          </p>
          {operation.value.recovery.length > 0 ? (
            <ul>
              {operation.value.recovery.map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}

      {plan ? <PlanReview plan={plan} /> : null}

      {currentExport ? (
        <div className={styles.planBox}>
          <h3>Exported Operation Plan</h3>
          <p className={styles.muted}>Exact persisted plan · digest {currentExport.planDigest}</p>
          <pre className={styles.planJson}>{currentExport.json}</pre>
          <SecondaryButton onPress={() => downloadPlan(currentExport)}>
            Download JSON
          </SecondaryButton>
        </div>
      ) : null}
      {exportPlan.isError ? <ErrorBanner error={exportPlan.error} /> : null}

      <div className={styles.panelActions}>
        {plan ? (
          <SecondaryButton
            onPress={() => exportPlan.mutate(plan.plan.operationId)}
            isDisabled={busy || exportPlan.isPending}
          >
            {exportPlan.isPending ? 'Exporting…' : 'Export plan JSON'}
          </SecondaryButton>
        ) : null}
        {plan && !terminal ? (
          <PrimaryButton
            onPress={onExecute}
            isDisabled={
              busy || !executionAllowed(plan) || (plan.kind === 'trash' && !currentExport)
            }
          >
            {busy ? 'Running…' : 'Execute reviewed plan'}
          </PrimaryButton>
        ) : null}
        {operation?.value.cancellationAllowed ? (
          <DangerButton
            onPress={() => {
              onCancel();
              restoreFocus();
            }}
            isDisabled={busy}
          >
            Request cancel
          </DangerButton>
        ) : null}
        <SecondaryButton
          onPress={() => {
            onClear();
            restoreFocus();
          }}
          isDisabled={busy}
        >
          Dismiss
        </SecondaryButton>
      </div>
    </div>
  );
}

function downloadPlan(view: { operationId: string; json: string }) {
  const url = URL.createObjectURL(new Blob([view.json], { type: 'application/json' }));
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = `skills-hub-operation-${view.operationId}.json`;
  anchor.click();
  URL.revokeObjectURL(url);
}

function PlanReview({ plan }: { plan: ReviewedPlan }) {
  if (plan.kind === 'trash') {
    const view = plan.plan;
    return (
      <div className={styles.planBox}>
        <h3>Trash plan</h3>
        <dl className={styles.metaList}>
          <MetaRow label="Action" value={plan.action.replaceAll('_', ' ')} />
          <MetaRow label="Skill" value={view.entry.displayName} />
          <MetaRow
            label="Original path"
            value={<PathText path={view.entry.originalWorkingPath} />}
          />
          <MetaRow label="Retention" value={view.entry.retentionPolicy} />
        </dl>
        {view.blockers.map((blocker) => (
          <p key={blocker.code}>Blocker: {blocker.detail}</p>
        ))}
      </div>
    );
  }
  if (plan.kind === 'takeover') {
    const view = plan.plan;
    return (
      <div className={styles.planBox}>
        <h3>Takeover plan</h3>
        <dl className={styles.metaList}>
          <MetaRow label="Decision" value={view.decision} />
          <MetaRow label="Skill" value={view.skillId} />
          <MetaRow label="Working path" value={<PathText path={view.workingPath} />} />
          <MetaRow label="Digest" value={view.reviewedDigest} />
          <MetaRow label="Recovery" value={view.recoverySummary} />
          <MetaRow label="Consequence" value={view.crossVolumeConsequence ?? 'Same-volume only'} />
        </dl>
        <ul>
          {view.observations.map((observation) => (
            <li key={observation.observationId}>
              {observation.path} · {observation.status}
            </li>
          ))}
          {view.selectedReplacements.map((replacement) => (
            <li key={replacement.deploymentId}>
              Replace {replacement.path} as {replacement.resolvedMode}
              {replacement.fallbackReason ? ` (${replacement.fallbackReason})` : ''}
            </li>
          ))}
          {view.blockers.map((blocker) => (
            <li key={blocker}>Blocker: {blocker}</li>
          ))}
        </ul>
      </div>
    );
  }

  if (plan.kind === 'batch') {
    const view = plan.plan;
    return (
      <div className={styles.planBox}>
        <h3>Batch deployment plan</h3>
        <dl className={styles.metaList}>
          <MetaRow label="Action" value={view.action} />
          <MetaRow label="Skill" value={view.skillId} />
          <MetaRow label="Targets" value={String(view.entries.length)} />
          <MetaRow label="Consequence" value={view.consequence} />
        </dl>
        <ul>
          {view.entries.map((entry) => (
            <li key={entry.deploymentId}>
              {entry.targetPath} · {entry.resolvedMode} · {entry.consequence}
            </li>
          ))}
        </ul>
      </div>
    );
  }

  const view = plan.plan;
  return (
    <div className={styles.planBox}>
      <h3>Deployment plan</h3>
      <dl className={styles.metaList}>
        <MetaRow label="Action" value={view.action} />
        <MetaRow label="Target path" value={<PathText path={view.targetPath} />} />
        <MetaRow label="Requested mode" value={view.requestedMode} />
        <MetaRow label="Resolved mode" value={view.resolvedMode} />
        <MetaRow label="Health" value={view.reviewedHealth} />
        <MetaRow label="Consequence" value={view.consequence} />
        <MetaRow label="Fallback" value={view.fallbackReason ?? 'None'} />
      </dl>
    </div>
  );
}

function executionAllowed(plan: ReviewedPlan): boolean {
  return plan.plan.executionAllowed;
}
