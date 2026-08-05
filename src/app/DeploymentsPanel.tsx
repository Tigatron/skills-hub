import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import { Button } from 'react-aria-components';

import type { AnyOperationView, DeploymentHealthView, FixtureTargetKindDto } from '../bindings';
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

export function DeploymentsPanel() {
  const queryClient = useQueryClient();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [includeInactive, setIncludeInactive] = useState(false);
  const [fixtureKind, setFixtureKind] = useState<FixtureTargetKindDto>('global');
  const [fixturePath, setFixturePath] = useState('');
  const [plan, setPlan] = useState<ReviewedPlan | null>(null);
  const [operation, setOperation] = useState<AnyOperationView | null>(null);

  const deployments = useQuery({
    queryKey: queryKeys.deployments({ skillId: null, includeInactive }),
    queryFn: () =>
      api.deploymentsList({
        skillId: null,
        targetId: null,
        includeInactive,
        limit: 100,
      }),
  });

  const targets = useQuery({
    queryKey: queryKeys.targets,
    queryFn: () => api.targetsList(),
  });

  const selected = useMemo(
    () => deployments.data?.items.find((item) => item.deploymentId === selectedId) ?? null,
    [deployments.data, selectedId],
  );

  const registerTarget = useMutation({
    mutationFn: () =>
      api.targetRegisterFixture({
        kind: fixtureKind,
        selectedDirectory: fixturePath.trim(),
      }),
    onSuccess: async () => {
      setFixturePath('');
      await queryClient.invalidateQueries({ queryKey: queryKeys.targets });
    },
  });

  const verify = useMutation({
    mutationFn: async () => {
      if (!selected) {
        throw new Error('Select a deployment first.');
      }
      return api.deploymentVerify(selected.deploymentId);
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['deployments'] });
    },
  });

  const planUndeploy = useMutation({
    mutationFn: async (resolution: 'remove_managed' | 'preserve_target') => {
      if (!selected) {
        throw new Error('Select a deployment first.');
      }
      return api.undeployPlan({
        deploymentId: selected.deploymentId,
        resolution,
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
      if (!operationId) {
        throw new Error('No operation to cancel.');
      }
      return api.operationCancel({ operationId });
    },
  });

  const busy =
    registerTarget.isPending ||
    verify.isPending ||
    planUndeploy.isPending ||
    execute.isPending ||
    cancel.isPending;

  return (
    <div className={styles.split}>
      <section className={styles.panel} aria-label="Deployments">
        <PanelHeader
          title="Deployments"
          description="Rust-owned health, drift direction, and allowed actions."
          actions={
            <label className={styles.inlineFields}>
              <input
                type="checkbox"
                checked={includeInactive}
                onChange={(event) => setIncludeInactive(event.target.checked)}
              />
              <span>Include inactive</span>
            </label>
          }
        />
        <div className={styles.panelBody}>
          {deployments.isError ? (
            <ErrorBanner error={deployments.error} onRetry={() => void deployments.refetch()} />
          ) : null}
          {deployments.isPending ? <LoadingBlock label="Loading deployments" /> : null}
          {deployments.data && deployments.data.items.length === 0 ? (
            <EmptyState
              title="No deployments yet"
              body="Deploy a Vaulted Skill from Library after reviewing a plan."
            />
          ) : null}
          <div className={styles.list} role="listbox" aria-label="Deployment rows">
            {deployments.data?.items.map((item) => (
              <Button
                key={item.deploymentId}
                className={styles.listItem!}
                onPress={() => setSelectedId(item.deploymentId)}
                data-selected={selectedId === item.deploymentId}
              >
                <div className={styles.listItemTitle}>
                  <span>{item.deploymentName}</span>
                  <StatusPill tone={healthTone(item.health)}>{item.health}</StatusPill>
                </div>
                <div className={styles.listItemMeta}>
                  {item.mode} · {item.driftDirection} · {item.targetPath}
                </div>
              </Button>
            ))}
          </div>
        </div>
      </section>

      <section className={styles.panel} aria-label="Deployment detail">
        <PanelHeader
          title="Target & undeploy"
          description="Fixture targets exist for thin-slice testing."
        />
        <div className={styles.panelBody}>
          <div className={styles.planBox}>
            <h3>Register fixture target</h3>
            <div className={styles.inlineFields}>
              <select
                className={styles.selectInput}
                value={fixtureKind}
                onChange={(event) => setFixtureKind(event.target.value as FixtureTargetKindDto)}
                aria-label="Fixture target kind"
              >
                <option value="global">Global</option>
                <option value="git_project">Git project</option>
                <option value="personal_project">Personal project</option>
              </select>
              <input
                className={styles.textInput}
                style={{ minWidth: 220 }}
                value={fixturePath}
                onChange={(event) => setFixturePath(event.target.value)}
                placeholder="/absolute/target/root"
                aria-label="Fixture target directory"
              />
              <PrimaryButton
                onPress={() => registerTarget.mutate()}
                isDisabled={busy || !fixturePath.trim()}
              >
                Register
              </PrimaryButton>
            </div>
            {registerTarget.isError ? <ErrorBanner error={registerTarget.error} /> : null}
            {targets.data && targets.data.length > 0 ? (
              <ul>
                {targets.data.map((target) => (
                  <li key={target.targetId}>
                    {target.scope} · default {target.defaultMode} · {target.rootPath}
                  </li>
                ))}
              </ul>
            ) : (
              <p className={styles.muted}>No targets registered in this Vault yet.</p>
            )}
          </div>

          {selected ? (
            <DeploymentDetail item={selected} />
          ) : (
            <EmptyState
              title="Select a deployment"
              body="Verify and undeploy actions stay plan-gated."
            />
          )}

          <div className={styles.panelActions}>
            <SecondaryButton onPress={() => verify.mutate()} isDisabled={!selected || busy}>
              Verify selected
            </SecondaryButton>
            <SecondaryButton
              onPress={() => planUndeploy.mutate('remove_managed')}
              isDisabled={!selected || busy}
            >
              Plan clean undeploy
            </SecondaryButton>
            <SecondaryButton
              onPress={() => planUndeploy.mutate('preserve_target')}
              isDisabled={!selected || busy}
            >
              Plan preserve undeploy
            </SecondaryButton>
          </div>

          {verify.isError ? <ErrorBanner error={verify.error} /> : null}
          {planUndeploy.isError ? <ErrorBanner error={planUndeploy.error} /> : null}
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
        </div>
      </section>
    </div>
  );
}

function DeploymentDetail({ item }: { item: DeploymentHealthView }) {
  return (
    <div className={styles.detailCard} style={{ padding: 0 }}>
      <h3>{item.deploymentName}</h3>
      <dl className={styles.metaList}>
        <MetaRow
          label="Health"
          value={<StatusPill tone={healthTone(item.health)}>{item.health}</StatusPill>}
        />
        <MetaRow label="Explanation" value={item.explanation} />
        <MetaRow label="Path" value={<PathText path={item.targetPath} />} />
        <MetaRow label="Mode" value={item.mode} />
        <MetaRow label="Drift" value={item.driftDirection} />
        <MetaRow label="Expected digest" value={item.expectedDigest} />
        <MetaRow label="Vault digest" value={item.vaultDigest ?? '—'} />
        <MetaRow label="Target digest" value={item.targetDigest ?? '—'} />
        <MetaRow label="Allowed" value={item.allowedActions.join(', ') || 'None'} />
        <MetaRow label="Disabled" value={item.disabledReason ?? '—'} />
      </dl>
    </div>
  );
}

function healthTone(health: string): 'neutral' | 'success' | 'pending' | 'danger' {
  const value = health.toLowerCase();
  if (value.includes('clean') || value.includes('ok')) {
    return 'success';
  }
  if (value.includes('missing') || value.includes('broken') || value.includes('conflict')) {
    return 'danger';
  }
  if (value.includes('ahead') || value.includes('modified') || value.includes('drift')) {
    return 'pending';
  }
  return 'neutral';
}
