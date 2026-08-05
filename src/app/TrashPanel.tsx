import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { Button } from 'react-aria-components';

import type {
  PlanExportView,
  TrashEntryView,
  TrashExecutionView,
  TrashPlanView,
} from '../bindings';
import { api } from '../lib/api';
import { queryKeys } from '../lib/query';
import {
  DangerButton,
  EmptyState,
  ErrorBanner,
  LoadingBlock,
  MetaRow,
  PanelHeader,
  PathText,
  PrimaryButton,
  SecondaryButton,
  StatusPill,
} from './components';
import styles from './TrashPanel.module.css';

type TrashAction = 'restore' | 'permanently_delete';
type ReviewedTrashPlan = {
  action: TrashAction;
  plan: TrashPlanView;
  export: PlanExportView | null;
};

export function TrashPanel() {
  const queryClient = useQueryClient();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState('');
  const [review, setReview] = useState<ReviewedTrashPlan | null>(null);
  const [outcome, setOutcome] = useState<TrashExecutionView | null>(null);

  const entries = useQuery({ queryKey: queryKeys.trash, queryFn: api.trashEntriesList });
  const retention = useQuery({
    queryKey: queryKeys.trashRetention,
    queryFn: api.trashRetentionSummary,
  });
  const selected = entries.data?.find((entry) => entry.entryId === selectedId) ?? null;

  useEffect(() => {
    if (!selectedId && entries.data?.[0]) setSelectedId(entries.data[0].entryId);
  }, [entries.data, selectedId]);

  useEffect(() => {
    setConfirmation('');
    setReview(null);
    setOutcome(null);
  }, [selectedId]);

  const plan = useMutation({
    mutationFn: async (action: TrashAction) => {
      if (!selected) throw new Error('Select a Trash entry first.');
      let planned: TrashPlanView;
      if (action === 'restore') {
        planned = await api.trashRestorePlan(selected.entryId);
      } else {
        if (confirmation !== selected.displayName) {
          throw new Error('Enter the exact Skill display name before planning permanent deletion.');
        }
        planned = await api.trashPermanentDeletePlan(selected.entryId, confirmation);
      }
      const exported = planned.operationId
        ? await api.operationPlanExport({ operationId: planned.operationId })
        : null;
      return { action, plan: planned, export: exported };
    },
    onSuccess: (reviewed) => {
      setReview(reviewed);
      setOutcome(null);
    },
  });

  const execute = useMutation({
    mutationFn: async () => {
      if (!review) throw new Error('Create and review a plan before execution.');
      return api.trashExecute(review.action, {
        operationId: review.plan.operationId,
        planDigest: review.plan.planDigest,
      });
    },
    onSuccess: async (result) => {
      setOutcome(result);
      if (result.outcome !== 'succeeded') return;
      setReview(null);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.trash }),
        queryClient.invalidateQueries({ queryKey: queryKeys.trashRetention }),
        queryClient.invalidateQueries({ queryKey: ['library'] }),
        queryClient.invalidateQueries({ queryKey: ['skill'] }),
        queryClient.invalidateQueries({ queryKey: ['activity'] }),
      ]);
    },
  });

  const busy = plan.isPending || execute.isPending;
  const error = entries.error ?? retention.error ?? plan.error ?? execute.error;

  return (
    <div className={styles.layout}>
      <section className={styles.panel} aria-label="Trash entries">
        <PanelHeader
          title="Trash"
          description="Removed Skills stay recoverable until you review and execute a permanent deletion. Trash is not undeploy: deployments are managed separately."
        />
        <div className={styles.body}>
          {retention.isLoading ? (
            <LoadingBlock label="Loading Trash retention summary" />
          ) : retention.data ? (
            <dl className={styles.summary} aria-label="Trash retention summary">
              <MetaRow label="Entries" value={String(retention.data.totalEntries)} />
              <MetaRow label="Expired" value={String(retention.data.expiredEntries)} />
              <MetaRow label="Protected" value={String(retention.data.protectedEntries)} />
              <MetaRow label="Next deadline" value={formatDate(retention.data.nextDeadline)} />
            </dl>
          ) : null}
          {error ? (
            <ErrorBanner
              error={error}
              onRetry={() => void refetchAll(entries.refetch, retention.refetch)}
            />
          ) : null}
          {entries.isLoading ? <LoadingBlock label="Loading Trash entries" /> : null}
          {!entries.isLoading && entries.data?.length === 0 ? (
            <EmptyState
              title="Trash is empty"
              body="Skills moved to Trash will appear here. Undeploying a Skill does not move it to Trash."
            />
          ) : null}
          {entries.data?.length ? (
            <div className={styles.list} role="list" aria-label="Trashed Skills">
              {entries.data.map((entry) => (
                <Button
                  key={entry.entryId}
                  className={styles.listItem!}
                  data-selected={entry.entryId === selectedId}
                  onPress={() => setSelectedId(entry.entryId)}
                >
                  <span className={styles.itemTop}>
                    <strong>{entry.displayName}</strong>
                    {entry.protectedReferences.length ? (
                      <StatusPill tone="pending">Protected</StatusPill>
                    ) : null}
                  </span>
                  <span className={styles.itemMeta}>Trashed {formatDate(entry.trashedAt)}</span>
                  <span className={styles.path} title={entry.originalWorkingPath}>
                    {entry.originalWorkingPath}
                  </span>
                </Button>
              ))}
            </div>
          ) : null}
        </div>
      </section>

      <section className={styles.panel} aria-label="Trash detail">
        <PanelHeader
          title={selected?.displayName ?? 'Trash detail'}
          description="Review identity, references, and retention before taking action."
        />
        <div className={styles.body}>
          {!selected ? (
            <EmptyState
              title="No Trash entry selected"
              body="Select a trashed Skill to inspect it."
            />
          ) : (
            <TrashDetail
              entry={selected}
              confirmation={confirmation}
              onConfirmationChange={setConfirmation}
              onPlan={(action) => plan.mutate(action)}
              busy={busy}
            />
          )}

          {review ? (
            <PlanReview
              review={review}
              busy={busy}
              onExecute={() => execute.mutate()}
              onDismiss={() => setReview(null)}
            />
          ) : null}
          {outcome ? <ExecutionOutcome outcome={outcome} /> : null}
        </div>
      </section>
    </div>
  );
}

function TrashDetail({
  entry,
  confirmation,
  onConfirmationChange,
  onPlan,
  busy,
}: {
  entry: TrashEntryView;
  confirmation: string;
  onConfirmationChange: (value: string) => void;
  onPlan: (action: TrashAction) => void;
  busy: boolean;
}) {
  return (
    <div className={styles.stack}>
      <dl className={styles.details}>
        <MetaRow label="Original path" value={<PathText path={entry.originalWorkingPath} />} />
        <MetaRow label="Trashed" value={formatDate(entry.trashedAt)} />
        <MetaRow label="Retention deadline" value={formatDate(entry.retentionDeadline)} />
        <MetaRow label="Retention policy" value={entry.retentionPolicy} />
      </dl>
      <div className={styles.references}>
        <h3>Protected references</h3>
        {entry.protectedReferences.length ? (
          <ul>
            {entry.protectedReferences.map((reference) => (
              <li key={reference}>{reference}</li>
            ))}
          </ul>
        ) : (
          <p>None. Permanent deletion still requires a reviewed plan.</p>
        )}
      </div>
      <div className={styles.actions}>
        <PrimaryButton onPress={() => onPlan('restore')} isDisabled={busy}>
          Plan restore
        </PrimaryButton>
      </div>
      <div className={styles.dangerZone}>
        <h3>Permanent deletion</h3>
        <p>
          This removes the trashed working copy. It does not undeploy targets. Protected references
          may block execution.
        </p>
        <label htmlFor={`delete-${entry.entryId}`}>
          Enter <strong>{entry.displayName}</strong> exactly
        </label>
        <input
          id={`delete-${entry.entryId}`}
          className={styles.input}
          value={confirmation}
          onChange={(event) => onConfirmationChange(event.target.value)}
          autoComplete="off"
        />
        <DangerButton
          onPress={() => onPlan('permanently_delete')}
          isDisabled={busy || confirmation !== entry.displayName}
        >
          Plan permanent delete
        </DangerButton>
      </div>
    </div>
  );
}

function PlanReview({
  review,
  busy,
  onExecute,
  onDismiss,
}: {
  review: ReviewedTrashPlan;
  busy: boolean;
  onExecute: () => void;
  onDismiss: () => void;
}) {
  return (
    <div className={styles.review} aria-label="Reviewed Trash plan">
      <h3>{review.action === 'restore' ? 'Restore plan' : 'Permanent deletion plan'}</h3>
      <dl className={styles.details}>
        <MetaRow label="Skill" value={review.plan.entry.displayName} />
        <MetaRow
          label="Original path"
          value={<PathText path={review.plan.entry.originalWorkingPath} />}
        />
        <MetaRow label="Operation" value={review.plan.operationId} />
      </dl>
      {review.export ? (
        <div>
          <strong>Exact persisted plan</strong>
          <p>Digest {review.export.planDigest}</p>
          <pre className={styles.planJson}>{review.export.json}</pre>
        </div>
      ) : null}
      {review.plan.blockers.length ? (
        <div role="alert">
          <strong>Execution blocked</strong>
          <ul>
            {review.plan.blockers.map((blocker) => (
              <li key={blocker.code}>
                {blocker.detail}
                {blocker.deploymentIds.length ? ` (${blocker.deploymentIds.join(', ')})` : ''}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p>No blockers reported by the backend plan.</p>
      )}
      <div className={styles.actions}>
        {review.action === 'permanently_delete' ? (
          <DangerButton
            onPress={onExecute}
            isDisabled={busy || review.plan.blockers.length > 0 || !exportMatches(review)}
          >
            Execute reviewed permanent deletion
          </DangerButton>
        ) : (
          <PrimaryButton
            onPress={onExecute}
            isDisabled={busy || review.plan.blockers.length > 0 || !exportMatches(review)}
          >
            Execute reviewed restore
          </PrimaryButton>
        )}
        <SecondaryButton onPress={onDismiss} isDisabled={busy}>
          Dismiss plan
        </SecondaryButton>
      </div>
    </div>
  );
}

function ExecutionOutcome({ outcome }: { outcome: TrashExecutionView }) {
  const succeeded = outcome.outcome === 'succeeded';
  return (
    <div className={styles.outcome} role="status">
      <div className={styles.itemTop}>
        <strong>Trash operation outcome</strong>
        <StatusPill tone={succeeded ? 'success' : 'danger'}>{outcome.outcome}</StatusPill>
      </div>
      <p>
        Operation {outcome.operationId}
        {outcome.replayed ? ' · durable outcome replayed' : ' · durable outcome recorded'}
        {succeeded ? '' : ' · reviewed plan retained for recovery or inspection'}
      </p>
    </div>
  );
}

function exportMatches(review: ReviewedTrashPlan) {
  return (
    review.export?.operationId === review.plan.operationId &&
    review.export.planDigest === review.plan.planDigest
  );
}

function formatDate(value: string | null): string {
  if (!value) return 'None scheduled';
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}

async function refetchAll(...refetches: Array<() => Promise<unknown>>) {
  await Promise.all(refetches.map((refetch) => refetch()));
}
