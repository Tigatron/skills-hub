import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { Button } from 'react-aria-components';

import type { AnyOperationView, DeploymentHealthView, TargetView } from '../bindings';
import { api, type ReviewedPlan } from '../lib/api';
import { invalidateAfterOperation, queryKeys } from '../lib/query';
import {
  EmptyState,
  ErrorBanner,
  LoadingBlock,
  MetaRow,
  PanelHeader,
  PathText,
  SecondaryButton,
  StatusPill,
} from './components';
import { OperationPanel } from './OperationPanel';
import styles from './DeploymentsPanel.module.css';

type GroupBy = 'agent' | 'project' | 'skill';
type DisplayMode = 'matrix' | 'list';
type DeploymentGroup = { id: string; label: string; items: DeploymentHealthView[] };

export function DeploymentsPanel() {
  const queryClient = useQueryClient();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [includeInactive, setIncludeInactive] = useState(false);
  const [groupBy, setGroupBy] = useState<GroupBy>('agent');
  const [displayMode, setDisplayMode] = useState<DisplayMode>('matrix');
  const [plan, setPlan] = useState<ReviewedPlan | null>(null);
  const [operation, setOperation] = useState<AnyOperationView | null>(null);

  const deployments = useQuery({
    queryKey: queryKeys.deployments({ skillId: null, includeInactive }),
    queryFn: () =>
      api.deploymentsList({ skillId: null, targetId: null, includeInactive, limit: 100 }),
  });
  const targets = useQuery({ queryKey: queryKeys.targets, queryFn: () => api.targetsList() });
  const selected = deployments.data?.items.find((item) => item.deploymentId === selectedId) ?? null;
  const groups = useMemo(
    () => groupDeployments(deployments.data?.items ?? [], targets.data ?? [], groupBy),
    [deployments.data, targets.data, groupBy],
  );

  const verify = useMutation({
    mutationFn: async () => {
      if (!selected) throw new Error('Select a deployment first.');
      return api.deploymentVerify(selected.deploymentId);
    },
    onSuccess: async () => queryClient.invalidateQueries({ queryKey: ['deployments'] }),
  });
  const planUndeploy = useMutation({
    mutationFn: async (resolution: 'remove_managed' | 'preserve_target') => {
      if (!selected) throw new Error('Select a deployment first.');
      return api.undeployPlan({ deploymentId: selected.deploymentId, resolution });
    },
    onSuccess: (reviewed) => {
      setPlan({ kind: 'deployment', plan: reviewed });
      setOperation(null);
    },
  });
  const planRedeploy = useMutation({
    mutationFn: async () => {
      if (!selected) throw new Error('Select a deployment first.');
      return api.deploymentPlan({
        skillId: selected.skillId,
        targetId: selected.targetId,
        requestedMode: selected.mode,
      });
    },
    onSuccess: (reviewed) => {
      setPlan({ kind: 'deployment', plan: reviewed });
      setOperation(null);
    },
  });
  const execute = useMutation({
    mutationFn: async () => {
      if (!plan) throw new Error('No plan to execute.');
      return api.operationExecute({
        operationId: plan.plan.operationId,
        planDigest: plan.plan.planDigest,
      });
    },
    onSuccess: async (view) => {
      setOperation(view);
      await invalidateAfterOperation(queryClient);
    },
  });
  const cancel = useMutation({
    mutationFn: async () => {
      const operationId = operation?.value.operationId ?? plan?.plan.operationId;
      if (!operationId) throw new Error('No operation to cancel.');
      return api.operationCancel({ operationId });
    },
  });
  const busy =
    verify.isPending ||
    planUndeploy.isPending ||
    planRedeploy.isPending ||
    execute.isPending ||
    cancel.isPending;

  return (
    <div className={styles.surface}>
      <section className={styles.browser} aria-label="Deployments">
        <PanelHeader
          title="Deployments"
          description="Backend-reported health, drift, and available actions."
          actions={
            <label className={styles.check}>
              <input
                type="checkbox"
                checked={includeInactive}
                onChange={(event) => setIncludeInactive(event.target.checked)}
              />
              Include inactive
            </label>
          }
        />
        <div className={styles.toolbar} aria-label="Deployment view controls">
          <label>
            Group by{' '}
            <select value={groupBy} onChange={(event) => setGroupBy(event.target.value as GroupBy)}>
              <option value="agent">Agent</option>
              <option value="project">Project</option>
              <option value="skill">Skill</option>
            </select>
          </label>
          <div className={styles.mode} aria-label="Display mode">
            <Button
              onPress={() => setDisplayMode('matrix')}
              aria-pressed={displayMode === 'matrix'}
            >
              Matrix
            </Button>
            <Button onPress={() => setDisplayMode('list')} aria-pressed={displayMode === 'list'}>
              List
            </Button>
          </div>
        </div>
        {deployments.isError ? (
          <ErrorBanner error={deployments.error} onRetry={() => void deployments.refetch()} />
        ) : null}
        {targets.isError ? (
          <ErrorBanner error={targets.error} onRetry={() => void targets.refetch()} />
        ) : null}
        {deployments.isPending || targets.isPending ? (
          <LoadingBlock label="Loading deployments" />
        ) : null}
        {deployments.data?.items.length === 0 ? (
          <EmptyState
            title="No deployments yet"
            body="Deploy a Vaulted Skill from Library after reviewing a plan."
          />
        ) : null}
        {displayMode === 'matrix' ? (
          <DeploymentMatrix groups={groups} selectedId={selectedId} onSelect={setSelectedId} />
        ) : (
          <DeploymentList groups={groups} selectedId={selectedId} onSelect={setSelectedId} />
        )}
      </section>

      <section className={styles.detail} aria-label="Deployment detail">
        <PanelHeader
          title="Deployment detail"
          description="Undeploy always starts with a reviewed plan."
        />
        {selected ? (
          <DeploymentDetail item={selected} />
        ) : (
          <EmptyState
            title="Select a deployment"
            body="Choose a row to inspect backend evidence and actions."
          />
        )}
        <ActionBar
          item={selected}
          busy={busy}
          onVerify={() => verify.mutate()}
          onRedeploy={() => planRedeploy.mutate()}
          onUndeploy={(resolution) => planUndeploy.mutate(resolution)}
        />
        {verify.isError ? <ErrorBanner error={verify.error} /> : null}
        {planUndeploy.isError ? <ErrorBanner error={planUndeploy.error} /> : null}
        {planRedeploy.isError ? <ErrorBanner error={planRedeploy.error} /> : null}
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
        />
      </section>
    </div>
  );
}

function DeploymentMatrix({ groups, selectedId, onSelect }: ViewProps) {
  return (
    <div className={styles.matrixWrap}>
      <table className={styles.matrix} aria-label="Deployment matrix">
        <thead>
          <tr>
            <th scope="col">Group / deployment</th>
            <th scope="col">Health</th>
            <th scope="col">Drift</th>
            <th scope="col">Target path</th>
            <th scope="col">Action</th>
          </tr>
        </thead>
        {groups.map((group) => (
          <tbody key={group.id}>
            <tr className={styles.group}>
              <th scope="rowgroup" colSpan={5}>
                {group.label}
              </th>
            </tr>
            {group.items.map((item) => (
              <tr key={item.deploymentId} data-selected={selectedId === item.deploymentId}>
                <th scope="row">{item.deploymentName}</th>
                <td>
                  <Health item={item} />
                </td>
                <td>{item.driftDirection}</td>
                <td>
                  <PathText path={item.targetPath} />
                </td>
                <td>
                  <Button
                    onPress={() => onSelect(item.deploymentId)}
                    aria-label={`Select ${item.deploymentName}`}
                  >
                    Select
                  </Button>
                </td>
              </tr>
            ))}
          </tbody>
        ))}
      </table>
    </div>
  );
}

function DeploymentList({ groups, selectedId, onSelect }: ViewProps) {
  return (
    <div className={styles.list} aria-label="Deployment list">
      {groups.map((group) => (
        <section key={group.id}>
          <h3>{group.label}</h3>
          {group.items.map((item) => (
            <article
              key={item.deploymentId}
              data-selected={selectedId === item.deploymentId}
              className={styles.card}
            >
              <div>
                <strong>{item.deploymentName}</strong>
                <Health item={item} />
              </div>
              <dl>
                <MetaRow label="Drift" value={item.driftDirection} />
                <MetaRow label="Target path" value={<PathText path={item.targetPath} />} />
              </dl>
              <Button
                onPress={() => onSelect(item.deploymentId)}
                aria-label={`Select ${item.deploymentName}`}
              >
                Select
              </Button>
            </article>
          ))}
        </section>
      ))}
    </div>
  );
}

type ViewProps = {
  groups: DeploymentGroup[];
  selectedId: string | null;
  onSelect: (id: string) => void;
};

function Health({ item }: { item: DeploymentHealthView }) {
  const tone =
    item.health === 'clean'
      ? 'success'
      : item.health === 'unverified' || item.health === 'vault_ahead'
        ? 'pending'
        : item.health === 'missing_target' ||
            item.health === 'broken_link' ||
            item.health === 'conflict' ||
            item.health === 'target_modified'
          ? 'danger'
          : 'neutral';
  return <StatusPill tone={tone}>{item.health}</StatusPill>;
}

function ActionBar({
  item,
  busy,
  onVerify,
  onRedeploy,
  onUndeploy,
}: {
  item: DeploymentHealthView | null;
  busy: boolean;
  onVerify: () => void;
  onRedeploy: () => void;
  onUndeploy: (resolution: 'remove_managed' | 'preserve_target') => void;
}) {
  const canVerify = Boolean(item?.allowedActions.includes('verify'));
  const canRedeploy = Boolean(item?.allowedActions.includes('redeploy'));
  const canClean = Boolean(item?.allowedActions.includes('undeploy'));
  const canPreserve = Boolean(item?.allowedActions.includes('undeploy_preserve'));
  return (
    <div className={styles.actions}>
      <SecondaryButton onPress={onVerify} isDisabled={!canVerify || busy}>
        Verify selected
      </SecondaryButton>
      <SecondaryButton onPress={onRedeploy} isDisabled={!canRedeploy || busy}>
        Plan redeploy
      </SecondaryButton>
      <SecondaryButton onPress={() => onUndeploy('remove_managed')} isDisabled={!canClean || busy}>
        Plan clean undeploy
      </SecondaryButton>
      <SecondaryButton
        onPress={() => onUndeploy('preserve_target')}
        isDisabled={!canPreserve || busy}
      >
        Plan preserve undeploy
      </SecondaryButton>
      {item?.disabledReason ? (
        <p className={styles.reason} role="note">
          Backend note: {item.disabledReason}
        </p>
      ) : null}
    </div>
  );
}

function DeploymentDetail({ item }: { item: DeploymentHealthView }) {
  return (
    <div className={styles.detailCard}>
      <h3>{item.deploymentName}</h3>
      <dl>
        <MetaRow label="Health" value={<Health item={item} />} />
        <MetaRow label="Explanation" value={item.explanation} />
        <MetaRow label="Path" value={<PathText path={item.targetPath} />} />
        <MetaRow label="Mode" value={item.mode} />
        <MetaRow label="Drift direction" value={item.driftDirection} />
        <MetaRow label="Expected digest" value={item.expectedDigest} />
        <MetaRow label="Vault digest" value={item.vaultDigest ?? '—'} />
        <MetaRow label="Target digest" value={item.targetDigest ?? '—'} />
        <MetaRow
          label="Allowed actions"
          value={item.allowedActions.join(', ') || 'None reported'}
        />
      </dl>
    </div>
  );
}

function groupDeployments(
  items: DeploymentHealthView[],
  targets: TargetView[],
  by: GroupBy,
): DeploymentGroup[] {
  const targetById = new Map(targets.map((target) => [target.targetId, target]));
  const groups = new Map<string, DeploymentGroup>();
  for (const item of items) {
    const target = targetById.get(item.targetId);
    let id = item.targetId;
    let label = target
      ? `${target.adapterId} — ${target.rootPath}`
      : `Agent target ${item.targetId}`;
    if (by === 'skill') {
      id = item.skillId;
      label = `Skill ${item.skillId}`;
    }
    if (by === 'project') {
      id = target?.projectId ?? 'global';
      label = target?.projectId
        ? `${target.projectKind ?? 'Project'} — ${target.projectId}`
        : 'Global';
    }
    const group = groups.get(id) ?? { id, label, items: [] };
    group.items.push(item);
    groups.set(id, group);
  }
  return [...groups.values()];
}
