import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { Button } from 'react-aria-components';

import { api } from '../lib/api';
import { queryKeys } from '../lib/query';
import {
  EmptyState,
  ErrorBanner,
  LoadingBlock,
  MetaRow,
  PanelHeader,
  PathText,
  StatusPill,
} from './components';
import styles from './thin.module.css';

export function ActivityPanel() {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [kind, setKind] = useState('');
  const [outcome, setOutcome] = useState('');

  const list = useQuery({
    queryKey: queryKeys.activity({
      kind: kind || null,
      outcome: outcome || null,
    }),
    queryFn: () =>
      api.activityList({
        kind: kind || null,
        outcome: outcome || null,
        limit: 100,
      }),
  });

  const selected = useMemo(
    () => list.data?.find((item) => item.id === selectedId) ?? null,
    [list.data, selectedId],
  );

  const detail = useQuery({
    queryKey: queryKeys.activityDetail(selectedId ?? 'none'),
    queryFn: () => api.activityDetail(selectedId!),
    enabled: Boolean(selectedId),
  });

  return (
    <div className={styles.split}>
      <section className={styles.panel} aria-label="Activity">
        <PanelHeader
          title="Activity"
          description="Append-only outcomes and recovery availability from Rust."
        />
        <div className={styles.panelBody}>
          <div className={styles.toolbar}>
            <select
              className={styles.selectInput}
              value={kind}
              onChange={(event) => setKind(event.target.value)}
              aria-label="Activity kind filter"
            >
              <option value="">All kinds</option>
              <option value="scan">scan</option>
              <option value="takeover">takeover</option>
              <option value="deploy">deploy</option>
              <option value="undeploy">undeploy</option>
              <option value="batch">batch</option>
            </select>
            <select
              className={styles.selectInput}
              value={outcome}
              onChange={(event) => setOutcome(event.target.value)}
              aria-label="Activity outcome filter"
            >
              <option value="">All outcomes</option>
              <option value="succeeded">succeeded</option>
              <option value="failed">failed</option>
              <option value="rolled_back">rolled_back</option>
              <option value="recovery_required">recovery_required</option>
            </select>
          </div>

          {list.isError ? (
            <ErrorBanner error={list.error} onRetry={() => void list.refetch()} />
          ) : null}
          {list.isPending ? <LoadingBlock label="Loading activity" /> : null}
          {list.data && list.data.length === 0 ? (
            <EmptyState
              title="No activity yet"
              body="Scans and Operations project here after they finish."
            />
          ) : null}

          <div className={styles.list} role="listbox" aria-label="Activity items">
            {list.data?.map((item) => (
              <Button
                key={item.id}
                className={styles.listItem!}
                onPress={() => setSelectedId(item.id)}
                data-selected={selectedId === item.id}
              >
                <div className={styles.listItemTitle}>
                  <span>{item.summary}</span>
                  <StatusPill tone={outcomeTone(item.outcome)}>
                    {item.outcome ?? item.state}
                  </StatusPill>
                </div>
                <div className={styles.listItemMeta}>
                  {item.kind} · {item.startedAt}
                  {item.completedAt ? ` → ${item.completedAt}` : ''}
                </div>
              </Button>
            ))}
          </div>
        </div>
      </section>

      <section className={styles.panel} aria-label="Activity detail">
        <PanelHeader
          title="Evidence"
          description="Recovery links survive toast dismissal and restart."
        />
        <div className={styles.panelBody}>
          {!selected ? (
            <EmptyState
              title="Select an activity row"
              body="Detail is loaded from the authoritative projection."
            />
          ) : null}
          {detail.isError ? (
            <ErrorBanner error={detail.error} onRetry={() => void detail.refetch()} />
          ) : null}
          {detail.isPending && selectedId ? <LoadingBlock label="Loading activity detail" /> : null}
          {detail.data ? (
            <div className={styles.stack}>
              <div className={styles.detailCard} style={{ padding: 0 }}>
                <h3>{detail.data.item.summary}</h3>
                <dl className={styles.metaList}>
                  <MetaRow label="Kind" value={detail.data.item.kind} />
                  <MetaRow label="State" value={detail.data.item.state} />
                  <MetaRow label="Outcome" value={detail.data.item.outcome ?? '—'} />
                  <MetaRow label="Operation" value={detail.data.item.operationId ?? '—'} />
                  <MetaRow label="Scan run" value={detail.data.item.scanRunId ?? '—'} />
                </dl>
              </div>

              {detail.data.operation ? (
                <div className={styles.planBox}>
                  <h3>Operation evidence</h3>
                  <dl className={styles.metaList}>
                    <MetaRow
                      label="Recovery"
                      value={
                        detail.data.operation.recoveryAvailable ? 'Available' : 'Not available'
                      }
                    />
                    <MetaRow label="Error" value={detail.data.operation.errorCode ?? '—'} />
                    <MetaRow
                      label="Failed step"
                      value={detail.data.operation.failedStep?.toString() ?? '—'}
                    />
                    <MetaRow
                      label="Plan"
                      value={<PathText path={detail.data.operation.planReference} />}
                    />
                    <MetaRow
                      label="Journal"
                      value={<PathText path={detail.data.operation.journalReference} />}
                    />
                  </dl>
                  <ul>
                    {detail.data.operation.paths.map((path) => (
                      <li key={`${path.stepOrder}-${path.path}`}>
                        #{path.stepOrder} {path.path}
                        {path.resolvedMode ? ` · ${path.resolvedMode}` : ''}
                      </li>
                    ))}
                    {detail.data.operation.recoveryReferences.map((reference) => (
                      <li key={reference}>Recovery: {reference}</li>
                    ))}
                  </ul>
                </div>
              ) : null}

              {detail.data.scan ? (
                <div className={styles.planBox}>
                  <h3>Scan diagnostics</h3>
                  <p className={styles.muted}>
                    {detail.data.scan.diagnosticCount} diagnostic
                    {detail.data.scan.diagnosticCount === 1 ? '' : 's'} aggregated without per-file
                    noise.
                  </p>
                  <ul>
                    {detail.data.scan.errorCodes.map((code) => (
                      <li key={code}>{code}</li>
                    ))}
                  </ul>
                </div>
              ) : null}
            </div>
          ) : null}
        </div>
      </section>
    </div>
  );
}

function outcomeTone(outcome: string | null): 'neutral' | 'success' | 'pending' | 'danger' {
  const value = (outcome ?? '').toLowerCase();
  if (value.includes('success')) {
    return 'success';
  }
  if (value.includes('fail') || value.includes('roll') || value.includes('recovery')) {
    return 'danger';
  }
  if (value) {
    return 'pending';
  }
  return 'neutral';
}
