import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useMemo, useRef, useState } from 'react';

import type {
  AnyOperationView,
  DeploymentHealthView,
  DeploymentModeDto,
  LibraryFilter,
  SkillDetail,
} from '../bindings';
import { api, type ReviewedPlan } from '../lib/api';
import { invalidateAfterOperation, queryKeys } from '../lib/query';
import {
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
import { OperationPanel } from './OperationPanel';
import styles from './thin.module.css';

const PAGE_SIZE = 100;

export function LibraryPanel() {
  const queryClient = useQueryClient();
  const [search, setSearch] = useState('');
  const [filter, setFilter] = useState<LibraryFilter>('all');
  const [offset, setOffset] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [scanJobId, setScanJobId] = useState<string | null>(null);
  const [plan, setPlan] = useState<ReviewedPlan | null>(null);
  const [operation, setOperation] = useState<AnyOperationView | null>(null);
  const [targetId, setTargetId] = useState('');
  const [mode, setMode] = useState<DeploymentModeDto | ''>('');
  const [manualSkillId, setManualSkillId] = useState('');
  const [deleteConfirmation, setDeleteConfirmation] = useState('');
  const [previewPath, setPreviewPath] = useState('SKILL.md');
  const [debouncedSearch, setDebouncedSearch] = useState('');
  const scanButtonRef = useRef<HTMLButtonElement | null>(null);
  const detailActionsRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const handle = window.setTimeout(() => setDebouncedSearch(search.trim()), 160);
    return () => window.clearTimeout(handle);
  }, [search]);

  const library = useQuery({
    queryKey: queryKeys.library({ search: debouncedSearch, filter, offset }),
    queryFn: () =>
      api.libraryList({
        offset,
        limit: PAGE_SIZE,
        search: debouncedSearch ? debouncedSearch : null,
        filter,
      }),
  });

  useEffect(() => setOffset(0), [debouncedSearch, filter]);

  const selected = useMemo(
    () => library.data?.items.find((item) => item.id === selectedId) ?? null,
    [library.data, selectedId],
  );
  const listRef = useRef<HTMLDivElement>(null);
  const rowVirtualizer = useVirtualizer({
    count: library.data?.items.length ?? 0,
    getScrollElement: () => listRef.current,
    estimateSize: () => 92,
    initialRect: { width: 640, height: 620 },
    overscan: 5,
  });

  const vaultedSkillId =
    selected?.skillId ?? operationSkillId(operation) ?? (manualSkillId.trim() || null);
  const skill = useQuery({
    queryKey: queryKeys.skill(vaultedSkillId ?? 'none'),
    queryFn: () => api.skillGet({ skillId: vaultedSkillId! }),
    enabled: Boolean(vaultedSkillId),
  });
  const skillDeployments = useQuery({
    queryKey: queryKeys.deployments({ skillId: vaultedSkillId, includeInactive: true }),
    queryFn: () =>
      api.deploymentsList({
        skillId: vaultedSkillId,
        targetId: null,
        includeInactive: true,
        limit: 100,
      }),
    enabled: Boolean(vaultedSkillId),
  });
  const trashEntries = useQuery({
    queryKey: queryKeys.trash,
    queryFn: () => api.trashEntriesList(),
  });
  const trashEntry = trashEntries.data?.find((entry) => entry.skillId === vaultedSkillId) ?? null;

  const targets = useQuery({
    queryKey: queryKeys.targets,
    queryFn: () => api.targetsList(),
  });

  const scan = useQuery({
    queryKey: queryKeys.scan(scanJobId ?? 'none'),
    queryFn: () => api.scanGet(scanJobId!),
    enabled: Boolean(scanJobId),
    refetchInterval: (query) => {
      const state = query.state.data?.state;
      if (!state) {
        return 700;
      }
      return state === 'running' || state === 'queued' ? 700 : false;
    },
  });

  useEffect(() => {
    if (scan.data?.state === 'completed' || scan.data?.state === 'completed_with_errors') {
      void queryClient.invalidateQueries({ queryKey: ['library'] });
    }
  }, [scan.data?.state, queryClient]);

  const startScan = useMutation({
    mutationFn: () => api.scanStart({ source: 'universal_global' }),
    onSuccess: (job) => {
      setScanJobId(job.jobId);
    },
  });

  const planTakeover = useMutation({
    mutationFn: async (decision: 'add_to_vault' | 'add_and_manage') => {
      if (!selected) {
        throw new Error('Select a Library item first.');
      }
      const source = selected.locations[0];
      if (!source) {
        throw new Error('The selected item has no observation locations.');
      }
      return api.takeoverPlan({
        sourceObservationId: source.observationId,
        decision,
        selectedLocations:
          decision === 'add_and_manage'
            ? selected.locations.map((location) => ({
                observationId: location.observationId,
                mode: 'symlink',
              }))
            : [],
      });
    },
    onSuccess: (reviewed) => {
      setPlan({ kind: 'takeover', plan: reviewed });
      setOperation(null);
    },
  });

  const keepExternal = useMutation({
    mutationFn: async () => {
      const observationId = selected?.locations[0]?.observationId;
      if (!observationId) throw new Error('Select an observed Skill first.');
      return api.takeoverKeepExternal({ observationId });
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['library'] });
    },
  });

  const preview = useMutation({
    mutationFn: async () => {
      if (!vaultedSkillId) throw new Error('Load a Vaulted Skill first.');
      return {
        skillId: vaultedSkillId,
        result: await api.skillPreviewFile({
          skillId: vaultedSkillId,
          relativePath: previewPath,
        }),
      };
    },
  });
  const planDeploy = useMutation({
    mutationFn: async () => {
      const skillId = vaultedSkillId;
      if (!skillId) {
        throw new Error('Deploy needs a Vaulted Skill ID from a completed takeover.');
      }
      if (!targetId) {
        throw new Error('Choose a target first.');
      }
      return api.deploymentPlan({
        skillId,
        targetId,
        requestedMode: mode === '' ? null : mode,
      });
    },
    onSuccess: (reviewed) => {
      setPlan({ kind: 'deployment', plan: reviewed });
      setOperation(null);
    },
  });

  const execute = useMutation({
    mutationFn: async () => {
      if (!plan) {
        throw new Error('No plan to execute.');
      }
      if (plan.kind === 'trash') {
        const result = await api.trashExecute(plan.action, {
          operationId: plan.plan.operationId,
          planDigest: plan.plan.planDigest,
        });
        if (result.outcome !== 'succeeded') {
          throw new Error(
            `Trash operation ended as ${result.outcome}; the reviewed plan remains available.`,
          );
        }
        return result;
      }
      return api.operationExecute({
        operationId: plan.plan.operationId,
        planDigest: plan.plan.planDigest,
      });
    },
    onSuccess: async (view) => {
      if (!('kind' in view)) {
        setOperation(null);
        setPlan(null);
        await queryClient.invalidateQueries({ queryKey: queryKeys.trash });
        await invalidateAfterOperation(queryClient);
        return;
      }
      setOperation(view);
      const skillId = operationSkillId(view);
      if (skillId) {
        setManualSkillId(skillId);
      }
      await invalidateAfterOperation(queryClient);
    },
  });

  const planTrash = useMutation({
    mutationFn: async (action: 'move_to_trash' | 'restore' | 'permanently_delete') => {
      if (!skill.data) throw new Error('Load a managed Skill first.');
      if (action === 'move_to_trash') return api.trashMovePlan(skill.data.skillId);
      if (!trashEntry) throw new Error('The Trash entry is unavailable. Refresh and try again.');
      if (action === 'restore') return api.trashRestorePlan(trashEntry.entryId);
      return api.trashPermanentDeletePlan(trashEntry.entryId, deleteConfirmation);
    },
    onSuccess: (reviewed, action) => {
      setPlan({ kind: 'trash', action, plan: reviewed });
      setOperation(null);
    },
  });

  const cancel = useMutation({
    mutationFn: async () => {
      const operationId = operation?.value.operationId ?? plan?.plan.operationId;
      if (!operationId) {
        throw new Error('No operation to cancel.');
      }
      return api.operationCancel({ operationId });
    },
  });

  const busy =
    startScan.isPending ||
    keepExternal.isPending ||
    planTakeover.isPending ||
    planDeploy.isPending ||
    planTrash.isPending ||
    execute.isPending ||
    cancel.isPending;

  return (
    <div className={styles.split}>
      <section className={styles.panel} aria-label="Library">
        <PanelHeader
          title="Library"
          description="Observed Skills from configured sources. Ownership and validation come only from Rust."
          actions={
            <PrimaryButton
              ref={scanButtonRef}
              onPress={() => startScan.mutate()}
              isDisabled={startScan.isPending}
            >
              {startScan.isPending ? 'Starting scan…' : 'Scan Universal global'}
            </PrimaryButton>
          }
        />
        <div className={styles.panelBody}>
          <div className={styles.toolbar}>
            <input
              className={styles.searchInput}
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder="Filter by name"
              aria-label="Filter library"
            />
            <select
              className={styles.selectInput}
              value={filter}
              onChange={(event) => setFilter(event.target.value as LibraryFilter)}
              aria-label="Library filter"
            >
              <option value="all">All</option>
              <option value="verified">Verified</option>
              <option value="errors">Errors</option>
              <option value="conflicts">Conflicts</option>
              <option value="duplicates">Duplicates</option>
            </select>
          </div>

          {scanJobId ? (
            <div className={styles.operationBanner} role="status">
              <div className={styles.listItemTitle}>
                <strong>Scan {scan.data?.state ?? 'starting'}</strong>
                <StatusPill tone="pending">
                  {scan.data
                    ? `${scan.data.completedEntries}/${scan.data.estimatedEntries || '—'}`
                    : '…'}
                </StatusPill>
              </div>
              <p className={styles.muted}>
                {scan.data?.displayRoot ?? 'Universal global root'}
                {scan.data?.coverage.noFilesChanged ? ' · No files were changed.' : ''}
              </p>
            </div>
          ) : null}

          {library.isError ? (
            <ErrorBanner error={library.error} onRetry={() => void library.refetch()} />
          ) : null}
          {startScan.isError ? <ErrorBanner error={startScan.error} /> : null}
          {library.isPending ? <LoadingBlock label="Loading library" /> : null}

          {library.data && library.data.items.length === 0 ? (
            <EmptyState
              title="No Skills observed yet"
              body="Run a Universal global scan. The scan never mutates agent files."
            />
          ) : null}

          <div
            ref={listRef}
            className={styles.virtualList}
            role="listbox"
            aria-label="Library items"
            aria-multiselectable={false}
            data-testid="library-virtual-list"
          >
            <div className={styles.virtualCanvas} style={{ height: rowVirtualizer.getTotalSize() }}>
              {rowVirtualizer.getVirtualItems().map((virtualRow) => {
                const item = library.data!.items[virtualRow.index]!;
                const locationCount = item.locations.length + (item.workingLocation ? 1 : 0);
                const isSelected = selectedId === item.id;
                return (
                  <button
                    key={item.id}
                    type="button"
                    ref={rowVirtualizer.measureElement}
                    data-index={virtualRow.index}
                    className={styles.listItem}
                    role="option"
                    aria-selected={isSelected}
                    onClick={() => {
                      setSelectedId(item.id);
                      if (item.skillId) setManualSkillId(item.skillId);
                    }}
                    data-selected={isSelected}
                    style={{
                      position: 'absolute',
                      top: 0,
                      left: 0,
                      width: '100%',
                      transform: `translateY(${virtualRow.start}px)`,
                    }}
                  >
                    <div className={styles.listItemTitle}>
                      <span>{item.displayName}</span>
                      <StatusPill tone={item.validation === 'verified' ? 'success' : 'danger'}>
                        {item.validation}
                      </StatusPill>
                    </div>
                    <div className={styles.listItemMeta}>
                      {item.ownership} · {item.sourceSummary} · {locationCount} location
                      {locationCount === 1 ? '' : 's'} · {item.deploymentCount} deployment
                      {item.deploymentCount === 1 ? '' : 's'} · changed {item.changedAt}
                    </div>
                    <div className={styles.listItemMeta}>
                      Exact duplicates {item.duplicateSummary.exactDuplicateLocations} · name
                      conflicts {item.duplicateSummary.nameConflicts} · probable matches{' '}
                      {item.duplicateSummary.probableDuplicatesOrRenames}
                    </div>
                  </button>
                );
              })}
            </div>
          </div>
          {library.data && library.data.total > library.data.limit ? (
            <div className={styles.panelActions} aria-label="Library pagination">
              <span className={styles.muted}>
                {library.data.offset + 1}–
                {Math.min(library.data.offset + library.data.items.length, library.data.total)} of{' '}
                {library.data.total}
              </span>
              <SecondaryButton
                onPress={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
                isDisabled={offset === 0}
              >
                Previous
              </SecondaryButton>
              <SecondaryButton
                onPress={() => setOffset(offset + PAGE_SIZE)}
                isDisabled={offset + library.data.limit >= library.data.total}
              >
                Next
              </SecondaryButton>
            </div>
          ) : null}
        </div>
      </section>

      <section className={styles.panel} aria-label="Skill detail and plans">
        <PanelHeader
          title="Detail & plans"
          description="Review takeover and deployment plans before any filesystem mutation."
        />
        <div className={styles.panelBody}>
          {selected ? (
            <div className={styles.stack}>
              <div className={styles.detailCard} style={{ padding: 0 }}>
                <h3>{selected.displayName}</h3>
                <dl className={styles.metaList}>
                  <MetaRow label="Ownership" value={selected.ownership} />
                  <MetaRow label="Deployment name" value={selected.deploymentName} />
                  <MetaRow label="Digest" value={selected.digest ?? 'Unavailable'} />
                  <MetaRow
                    label="Locations"
                    value={
                      <>
                        {selected.workingLocation ? (
                          <div>
                            <PathText path={selected.workingLocation} />
                          </div>
                        ) : null}
                        {selected.locations.map((location) => (
                          <div key={location.observationId}>
                            <PathText path={location.path} />
                          </div>
                        ))}
                      </>
                    }
                  />
                </dl>
              </div>

              {selected.ownership === 'external' ? (
                <div className={styles.panelActions}>
                  <SecondaryButton
                    onPress={() => keepExternal.mutate()}
                    isDisabled={busy || !selected.nextActions.includes('keep_external')}
                  >
                    Keep external
                  </SecondaryButton>
                  <SecondaryButton
                    onPress={() => planTakeover.mutate('add_to_vault')}
                    isDisabled={busy || !selected.nextActions.includes('add_to_vault')}
                  >
                    Plan Add to Vault
                  </SecondaryButton>
                  <SecondaryButton
                    onPress={() => planTakeover.mutate('add_and_manage')}
                    isDisabled={busy || !selected.nextActions.includes('add_and_manage')}
                  >
                    Plan Add and manage
                  </SecondaryButton>
                </div>
              ) : null}
            </div>
          ) : (
            <EmptyState
              title="Select a Library row"
              body="Details and allowed actions come from the Rust read model."
            />
          )}

          {skill.data ? (
            <div className={styles.stack}>
              <SkillSummary
                detail={skill.data}
                deployments={skillDeployments.isSuccess ? skillDeployments.data.items : null}
                busy={busy}
                confirmation={deleteConfirmation}
                onConfirmationChange={setDeleteConfirmation}
                onPlan={(action) => planTrash.mutate(action)}
              />
              {skillDeployments.isPending ? (
                <LoadingBlock label="Loading Skill deployment health" />
              ) : null}
              {skillDeployments.isError ? <ErrorBanner error={skillDeployments.error} /> : null}
              <div className={styles.planBox}>
                <h3>Safe file preview</h3>
                <div className={styles.inlineFields}>
                  <input
                    className={styles.textInput}
                    value={previewPath}
                    onChange={(event) => setPreviewPath(event.target.value)}
                    aria-label="Bundle-relative preview path"
                  />
                  <SecondaryButton
                    onPress={() => preview.mutate()}
                    isDisabled={
                      !previewPath.trim() ||
                      preview.isPending ||
                      !capability(skill.data, 'preview')?.allowed
                    }
                  >
                    {preview.isPending ? 'Loading…' : 'Preview'}
                  </SecondaryButton>
                </div>
                {preview.data?.skillId === vaultedSkillId ? (
                  <pre className={styles.planJson}>
                    {preview.data.result.relativePath} · {preview.data.result.size} bytes{`\n\n`}
                    {preview.data.result.content}
                  </pre>
                ) : null}
                {preview.isError ? <ErrorBanner error={preview.error} /> : null}
              </div>
            </div>
          ) : null}

          <div className={styles.stack} ref={detailActionsRef}>
            <div className={styles.inlineFields}>
              <input
                className={styles.textInput}
                value={manualSkillId}
                onChange={(event) => setManualSkillId(event.target.value)}
                placeholder="Vaulted Skill ID (auto-filled after takeover)"
                aria-label="Vaulted Skill ID"
              />
              <select
                className={styles.selectInput}
                value={targetId}
                onChange={(event) => setTargetId(event.target.value)}
                aria-label="Deployment target"
              >
                <option value="">Select target…</option>
                {targets.data?.map((target) => (
                  <option key={target.targetId} value={target.targetId}>
                    {target.scope} · {target.rootPath}
                  </option>
                ))}
              </select>
              <select
                className={styles.selectInput}
                value={mode}
                onChange={(event) => setMode(event.target.value as DeploymentModeDto | '')}
                aria-label="Requested mode"
              >
                <option value="">Default mode</option>
                <option value="symlink">Symlink</option>
                <option value="managed_copy">Managed Copy</option>
              </select>
              <SecondaryButton
                onPress={() => planDeploy.mutate()}
                isDisabled={
                  busy ||
                  !targetId ||
                  !vaultedSkillId ||
                  (skill.data ? !capability(skill.data, 'deploy')?.allowed : false)
                }
              >
                Plan deploy
              </SecondaryButton>
            </div>
            {targets.isError ? <ErrorBanner error={targets.error} /> : null}
            {!targets.data?.length ? (
              <p className={styles.muted}>
                No targets yet. Register a fixture target from Deployments when you need a project
                or global destination.
              </p>
            ) : null}
          </div>

          {planTakeover.isError ? <ErrorBanner error={planTakeover.error} /> : null}
          {keepExternal.isError ? <ErrorBanner error={keepExternal.error} /> : null}
          {planDeploy.isError ? <ErrorBanner error={planDeploy.error} /> : null}
          {planTrash.isError ? <ErrorBanner error={planTrash.error} /> : null}
          {execute.isError ? <ErrorBanner error={execute.error} /> : null}

          <OperationPanel
            plan={plan}
            operation={operation}
            busy={busy}
            onExecute={() => execute.mutate()}
            onCancel={() => cancel.mutate()}
            onClear={() => {
              setPlan(null);
              setOperation(null);
            }}
            focusReturnRef={detailActionsRef}
          />
        </div>
      </section>
    </div>
  );
}

export function SkillSummary({
  detail,
  deployments = null,
  busy,
  confirmation,
  onConfirmationChange,
  onPlan,
}: {
  detail: SkillDetail;
  deployments?: DeploymentHealthView[] | null;
  busy: boolean;
  confirmation: string;
  onConfirmationChange: (value: string) => void;
  onPlan: (action: 'move_to_trash' | 'restore' | 'permanently_delete') => void;
}) {
  return (
    <div className={styles.planBox}>
      <h3>Vaulted Skill</h3>
      <dl className={styles.metaList}>
        <MetaRow label="Skill ID" value={detail.skillId} />
        <MetaRow label="Ownership" value={detail.ownership} />
        <MetaRow
          label="Working path"
          value={
            <PathText
              path={detail.workingPath}
              {...(capability(detail, 'reveal')?.allowed
                ? { onReveal: () => api.vaultRevealWorking(detail.skillId) }
                : {})}
            />
          }
        />
        <MetaRow label="Working digest" value={detail.workingDigest} />
        <MetaRow label="Baseline digest" value={detail.baselineDigest} />
        <MetaRow label="Lifecycle" value={detail.lifecycle} />
        <MetaRow
          label="Provenance"
          value={
            detail.sourcePaths.length
              ? detail.sourcePaths.map((path) => <PathText key={path} path={path} />)
              : 'No source paths recorded'
          }
        />
        <MetaRow
          label="Observations"
          value={
            detail.observationPaths.length
              ? detail.observationPaths.map((path) => <PathText key={path} path={path} />)
              : 'None'
          }
        />
        <MetaRow label="Conflicts" value={detail.conflicts.join(', ') || 'None'} />
        <MetaRow
          label="Snapshots"
          value={
            detail.snapshot.available
              ? `${detail.snapshot.count} retained · ${detail.snapshot.protectedCount} protected · latest ${detail.snapshot.latestCreatedAt ?? 'unknown'}`
              : (detail.snapshot.unavailableReason ?? 'Unavailable')
          }
        />
        <MetaRow
          label="Activity"
          value={
            detail.activity.length
              ? detail.activity.map((item) => (
                  <div key={item.activityId}>
                    {item.summary} · {item.outcome ?? item.kind} · {item.startedAt}
                    {item.undoCheckAvailable
                      ? ' · Undo plan check available'
                      : item.undoCheckReason
                        ? ` · Undo: ${item.undoCheckReason}`
                        : ''}
                  </div>
                ))
              : 'No linked activity'
          }
        />
        <MetaRow
          label="Deployments"
          value={
            detail.deploymentPaths.length
              ? detail.deploymentPaths.map((path) => (
                  <div key={path}>
                    <PathText path={path} />
                  </div>
                ))
              : 'None'
          }
        />
        {deployments ? (
          <MetaRow
            label="Deployment health"
            value={
              deployments.length
                ? deployments.map((deployment) => (
                    <div key={deployment.deploymentId}>
                      {deployment.health} · {deployment.driftDirection} ·{' '}
                      <PathText path={deployment.targetPath} />
                      {deployment.disabledReason ? ` · ${deployment.disabledReason}` : ''}
                    </div>
                  ))
                : 'None'
            }
          />
        ) : null}
      </dl>
      <div className={styles.panelActions}>
        {capability(detail, 'move_to_trash') ? (
          <SecondaryButton
            onPress={() => onPlan('move_to_trash')}
            isDisabled={busy || !capability(detail, 'move_to_trash')?.allowed}
          >
            Plan Move to Trash
          </SecondaryButton>
        ) : null}
        {capability(detail, 'restore') ? (
          <SecondaryButton
            onPress={() => onPlan('restore')}
            isDisabled={busy || !capability(detail, 'restore')?.allowed}
          >
            Plan restore
          </SecondaryButton>
        ) : null}
      </div>
      {detail.capabilities
        .filter((item) => !item.allowed && item.disabledReason)
        .map((item) => (
          <p className={styles.muted} key={item.action}>
            {item.action.replaceAll('_', ' ')} unavailable: {item.disabledReason}
          </p>
        ))}
      {capability(detail, 'permanently_delete')?.allowed ? (
        <div className={styles.stack}>
          <p className={styles.muted}>
            Permanent deletion cannot be undone. Type <strong>{detail.displayName}</strong> exactly
            to review the deletion plan.
          </p>
          <div className={styles.inlineFields}>
            <input
              className={styles.textInput}
              value={confirmation}
              onChange={(event) => onConfirmationChange(event.target.value)}
              aria-label={`Type ${detail.displayName} to confirm permanent deletion`}
            />
            <SecondaryButton
              onPress={() => onPlan('permanently_delete')}
              isDisabled={
                busy ||
                !capability(detail, 'permanently_delete')?.allowed ||
                confirmation !== detail.displayName
              }
            >
              Plan permanent delete
            </SecondaryButton>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function capability(detail: SkillDetail, action: SkillDetail['capabilities'][number]['action']) {
  return detail.capabilities.find((item) => item.action === action);
}

function operationSkillId(operation: AnyOperationView | null): string | null {
  if (!operation) {
    return null;
  }
  if (operation.kind === 'takeover') {
    return operation.value.context.skillId;
  }
  if (operation.kind === 'deployment') {
    return operation.value.review.skillId;
  }
  return operation.value.review.skillId;
}
