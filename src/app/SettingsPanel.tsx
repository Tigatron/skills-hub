import { useEffect, useState } from 'react';
import { type QueryClient, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Checkbox, Input, Label, TextArea, TextField } from 'react-aria-components';

import type {
  ConfiguredAdapterView,
  CustomTargetScope,
  DeploymentModeDto,
  ObjectGcPhase,
  WorkspaceRootView,
} from '../bindings';
import { api } from '../lib/api';
import { invalidateAfterOperation, queryKeys } from '../lib/query';
import {
  DangerButton,
  ErrorBanner,
  LoadingBlock,
  PathText,
  PrimaryButton,
  SecondaryButton,
  StatusPill,
} from './components';
import styles from './SettingsPanel.module.css';

type ReviewedLifecycle =
  | { kind: 'repair'; plan: Awaited<ReturnType<typeof api.vaultRepairPlan>> }
  | { kind: 'rebuild'; plan: Awaited<ReturnType<typeof api.vaultIndexRebuildPlan>> }
  | { kind: 'gc'; plan: Awaited<ReturnType<typeof api.vaultObjectGcPlan>> }
  | { kind: 'relocate'; plan: Awaited<ReturnType<typeof api.vaultRelocatePlan>> };

const date = (value: string | null) => (value ? new Date(value).toLocaleString() : 'Never');
const lines = (value: string) =>
  value
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);

export function SettingsPanel() {
  const client = useQueryClient();
  const vault = useQuery({ queryKey: queryKeys.vaultStatus, queryFn: api.vaultStatus });
  const adapters = useQuery({ queryKey: queryKeys.adapters, queryFn: api.adaptersList });
  const configured = useQuery({
    queryKey: queryKeys.configuredAdapters,
    queryFn: api.adaptersConfiguredList,
  });
  const roots = useQuery({ queryKey: queryKeys.workspaceRoots, queryFn: api.workspaceRootsList });
  const projects = useQuery({
    queryKey: queryKeys.manualProjects,
    queryFn: api.manualProjectsList,
  });
  const targets = useQuery({ queryKey: queryKeys.targets, queryFn: api.targetsList });
  const retention = useQuery({
    queryKey: queryKeys.trashRetention,
    queryFn: api.trashRetentionSummary,
  });
  const gcSettings = useQuery({
    queryKey: queryKeys.vaultGcSettings,
    queryFn: api.vaultObjectGcSettings,
  });
  const verify = useQuery({
    queryKey: queryKeys.vaultVerify,
    queryFn: api.vaultVerify,
    enabled: false,
  });
  const [review, setReview] = useState<ReviewedLifecycle | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const [relocatePath, setRelocatePath] = useState('');

  const plan = useMutation({
    mutationFn: async (action: 'repair' | 'rebuild' | ObjectGcPhase | 'relocate') => {
      if (action === 'repair')
        return { kind: 'repair', plan: await api.vaultRepairPlan() } as const;
      if (action === 'rebuild')
        return { kind: 'rebuild', plan: await api.vaultIndexRebuildPlan() } as const;
      if (action === 'relocate')
        return {
          kind: 'relocate',
          plan: await api.vaultRelocatePlan({ path: relocatePath }),
        } as const;
      return { kind: 'gc', plan: await api.vaultObjectGcPlan(action) } as const;
    },
    onSuccess: (next) => {
      setReview(next);
      setResult(null);
    },
  });
  const execute = useMutation({
    mutationFn: async (reviewed: ReviewedLifecycle) => {
      const identity = {
        operationId: reviewed.plan.operationId,
        planDigest: reviewed.plan.planDigest,
      };
      if (reviewed.kind === 'repair')
        return `${await api.vaultRepairExecute(identity)} repairs applied`;
      if (reviewed.kind === 'rebuild') {
        const value = await api.vaultIndexRebuildExecute(identity);
        return `Rebuilt ${value.rebuiltSkills} skills and ${value.rebuiltDeployments} deployments. Restart ${value.restartRequired ? 'required' : 'not required'}.`;
      }
      if (reviewed.kind === 'relocate') {
        const value = await api.vaultRelocateExecute(identity);
        return `Vault active at ${value.activeVaultPath}. ${value.rewrittenSymlinks} links updated. Restart ${value.restartRequired ? 'required' : 'not required'}; old Vault ${value.oldVaultRetained ? 'retained' : 'removed'}.`;
      }
      const value = await api.vaultObjectGcExecute(identity);
      return `${value.affectedObjects} objects affected (${value.phase}).`;
    },
    onSuccess: async (message) => {
      setResult(message);
      setReview(null);
      invalidateAfterOperation(client);
      await Promise.all([
        client.invalidateQueries({ queryKey: queryKeys.vaultStatus }),
        client.invalidateQueries({ queryKey: queryKeys.vaultVerify }),
        client.invalidateQueries({ queryKey: queryKeys.vaultGcSettings }),
      ]);
    },
  });
  const error = plan.error ?? execute.error;

  return (
    <div className={styles.settings}>
      <header className={styles.heading}>
        <h1>Settings</h1>
        <p>Vault integrity, discovery sources, and deployment destinations.</p>
      </header>

      <section className={styles.section} aria-labelledby="vault-heading">
        <div className={styles.sectionHeading}>
          <div>
            <h2 id="vault-heading">Vault</h2>
            <p>Review every filesystem change before it runs.</p>
          </div>
          <SecondaryButton onPress={() => void verify.refetch()} isDisabled={verify.isFetching}>
            Verify Vault
          </SecondaryButton>
        </div>
        {vault.isPending ? (
          <LoadingBlock label="Loading Vault status" />
        ) : vault.error ? (
          <ErrorBanner error={vault.error} onRetry={() => void vault.refetch()} />
        ) : (
          <div className={styles.summary}>
            <StatusPill tone={vault.data.initialized ? 'success' : 'danger'}>
              {vault.data.initialized ? 'Initialized' : 'Not initialized'}
            </StatusPill>
            <PathText path={vault.data.rootPath ?? vault.data.defaultPath} />
            <span>
              Startup recovery:{' '}
              {vault.data.startupRecoveryCompleted == null
                ? 'not reported'
                : vault.data.startupRecoveryCompleted
                  ? 'complete'
                  : 'incomplete'}
            </span>
          </div>
        )}
        {verify.data ? (
          <div className={styles.report}>
            <strong>
              {verify.data.healthy
                ? 'Vault is healthy'
                : `${verify.data.issues.length} integrity issue(s)`}
            </strong>
            <span>
              {verify.data.checkedSkills} skills · {verify.data.checkedObjects} objects checked
            </span>
            {verify.data.issues.map((issue) => (
              <div key={`${issue.code}:${issue.path}`} className={styles.diagnostic}>
                <b>{issue.code}</b> — {issue.detail}
                <PathText path={issue.path} />
              </div>
            ))}
          </div>
        ) : null}
        <div className={styles.actions}>
          <SecondaryButton
            onPress={() => plan.mutate('repair')}
            isDisabled={plan.isPending || execute.isPending}
          >
            Review repair
          </SecondaryButton>
          <SecondaryButton
            onPress={() => plan.mutate('rebuild')}
            isDisabled={plan.isPending || execute.isPending}
          >
            Review index rebuild
          </SecondaryButton>
          <SecondaryButton
            onPress={() => plan.mutate('stage_pending_delete')}
            isDisabled={plan.isPending || execute.isPending}
          >
            Review GC staging
          </SecondaryButton>
          <SecondaryButton
            onPress={() => plan.mutate('delete_pending')}
            isDisabled={plan.isPending || execute.isPending}
          >
            Review GC deletion
          </SecondaryButton>
          <TextField
            className={styles.inlineField!}
            value={relocatePath}
            onChange={setRelocatePath}
            aria-label="New Vault path"
          >
            <Input placeholder="New Vault path" />
          </TextField>
          <SecondaryButton
            onPress={() => plan.mutate('relocate')}
            isDisabled={!relocatePath || plan.isPending || execute.isPending}
          >
            Review relocation
          </SecondaryButton>
        </div>
        {gcSettings.data ? (
          <p className={styles.muted}>
            Object retention: {gcSettings.data.retentionDays} days · Last run{' '}
            {date(gcSettings.data.lastRun)} ·{' '}
            {gcSettings.data.eligible
              ? 'eligible'
              : `disabled: ${gcSettings.data.disabledReasons.join(', ')}`}
          </p>
        ) : null}
        {review ? (
          <PlanReview
            review={review}
            pending={execute.isPending}
            onCancel={() => setReview(null)}
            onExecute={() => execute.mutate(review)}
          />
        ) : null}
        {result ? (
          <p className={styles.success} role="status">
            {result}
          </p>
        ) : null}
        {error ? <ErrorBanner error={error} /> : null}
      </section>

      <AdaptersSection
        descriptors={adapters.data ?? []}
        configured={configured.data ?? []}
        loading={adapters.isPending || configured.isPending}
        error={adapters.error ?? configured.error}
      />
      <WorkspaceSection
        roots={roots.data ?? []}
        projects={projects.data ?? []}
        loading={roots.isPending || projects.isPending}
        error={roots.error ?? projects.error}
      />
      <TargetsSection
        targets={targets.data ?? []}
        projects={projects.data ?? []}
        loading={targets.isPending}
        error={targets.error}
      />

      <section className={styles.section} aria-labelledby="trash-retention-heading">
        <div className={styles.sectionHeading}>
          <div>
            <h2 id="trash-retention-heading">Trash retention</h2>
            <p>Protected entries are never removed by expiry.</p>
          </div>
        </div>
        {retention.isPending ? (
          <LoadingBlock label="Loading Trash retention" />
        ) : retention.error ? (
          <ErrorBanner error={retention.error} onRetry={() => void retention.refetch()} />
        ) : (
          <div className={styles.stats}>
            <span>
              <b>{retention.data.totalEntries}</b> entries
            </span>
            <span>
              <b>{retention.data.expiredEntries}</b> expired
            </span>
            <span>
              <b>{retention.data.protectedEntries}</b> protected
            </span>
            <span>Next deadline: {date(retention.data.nextDeadline)}</span>
          </div>
        )}
      </section>
    </div>
  );
}

function PlanReview({
  review,
  pending,
  onCancel,
  onExecute,
}: {
  review: ReviewedLifecycle;
  pending: boolean;
  onCancel: () => void;
  onExecute: () => void;
}) {
  const blockers =
    review.kind === 'repair'
      ? review.plan.refused.map((x) => x.detail)
      : review.kind === 'rebuild'
        ? review.plan.blockers.map((x) => x.detail)
        : review.kind === 'gc'
          ? review.plan.blockers
          : review.plan.capability.blockers;
  const allowed =
    review.kind === 'repair'
      ? review.plan.writable && blockers.length === 0
      : review.kind === 'rebuild'
        ? blockers.length === 0
        : review.kind === 'gc'
          ? review.plan.enabled && blockers.length === 0
          : review.plan.capability.status === 'supported' && blockers.length === 0;
  return (
    <div className={styles.review} aria-label={`${review.kind} plan review`}>
      <h3>Review {review.kind} plan</h3>
      <p>
        Operation <code>{review.plan.operationId}</code> · digest{' '}
        <code>{review.plan.planDigest}</code>
      </p>
      {review.kind === 'repair' ? <p>{review.plan.actions.length} safe action(s)</p> : null}
      {review.kind === 'gc' ? (
        <p>
          {review.plan.candidates.length} candidate(s); {review.plan.referencedObjects}/
          {review.plan.inspectedObjects} referenced
        </p>
      ) : null}
      {review.kind === 'relocate' ? (
        <>
          <p>
            Destination capability: <b>{review.plan.capability.status}</b>
          </p>
          <PathText path={review.plan.destinationPath} />
        </>
      ) : null}
      {blockers.length ? (
        <ul>
          {blockers.map((blocker) => (
            <li key={blocker}>{blocker}</li>
          ))}
        </ul>
      ) : (
        <p>No blockers reported.</p>
      )}
      <div className={styles.actions}>
        <PrimaryButton onPress={onExecute} isDisabled={!allowed || pending}>
          Execute reviewed plan
        </PrimaryButton>
        <SecondaryButton onPress={onCancel} isDisabled={pending}>
          Cancel
        </SecondaryButton>
      </div>
    </div>
  );
}

function AdaptersSection({
  descriptors,
  configured,
  loading,
  error,
}: {
  descriptors: Awaited<ReturnType<typeof api.adaptersList>>;
  configured: ConfiguredAdapterView[];
  loading: boolean;
  error: unknown;
}) {
  const client = useQueryClient();
  const [drafts, setDrafts] = useState<Record<string, ConfiguredAdapterView>>({});
  useEffect(
    () => setDrafts(Object.fromEntries(configured.map((item) => [item.adapterId, item]))),
    [configured],
  );
  const save = useMutation({
    mutationFn: (request: Parameters<typeof api.adapterConfigure>[0]) =>
      api.adapterConfigure(request),
    onSuccess: async () => {
      await Promise.all([
        client.invalidateQueries({ queryKey: queryKeys.configuredAdapters }),
        client.invalidateQueries({ queryKey: queryKeys.targets }),
      ]);
    },
  });
  return (
    <section className={styles.section} aria-labelledby="adapters-heading">
      <div className={styles.sectionHeading}>
        <div>
          <h2 id="adapters-heading">Adapters</h2>
          <p>Changes remain local until Save.</p>
        </div>
      </div>
      {loading ? (
        <LoadingBlock label="Loading adapters" />
      ) : error ? (
        <ErrorBanner error={error} />
      ) : (
        <div className={styles.cards}>
          {descriptors.map((descriptor) => {
            const current = configured.find((x) => x.adapterId === descriptor.adapterId) ?? {
              adapterId: descriptor.adapterId,
              displayName: descriptor.displayName,
              enabled: true,
              globalOverridePath: null,
              projectOverridePath: null,
            };
            const draft = drafts[descriptor.adapterId] ?? current;
            const change = (patch: Partial<ConfiguredAdapterView>) =>
              setDrafts((all) => ({ ...all, [descriptor.adapterId]: { ...draft, ...patch } }));
            return (
              <article className={styles.card} key={descriptor.adapterId}>
                <div className={styles.cardTitle}>
                  <h3>{descriptor.displayName}</h3>
                  <StatusPill
                    tone={
                      descriptor.confidence.toLowerCase().includes('verified')
                        ? 'success'
                        : 'pending'
                    }
                  >
                    {descriptor.confidence}
                  </StatusPill>
                </div>
                <p>{descriptor.caveats || 'No caveats reported.'}</p>
                <p className={styles.muted}>
                  Verified {date(descriptor.verifiedAt)} · {descriptor.supportedModes.join(', ')}
                </p>
                <Checkbox isSelected={draft.enabled} onChange={(enabled) => change({ enabled })}>
                  <span className={styles.checkbox} /> Enabled
                </Checkbox>
                <TextField
                  value={draft.globalOverridePath ?? ''}
                  onChange={(value) => change({ globalOverridePath: value || null })}
                >
                  <Label>Global override</Label>
                  <Input />
                </TextField>
                <TextField
                  value={draft.projectOverridePath ?? ''}
                  onChange={(value) => change({ projectOverridePath: value || null })}
                >
                  <Label>Project override</Label>
                  <Input />
                </TextField>
                <PrimaryButton
                  onPress={() =>
                    save.mutate({
                      adapterId: draft.adapterId,
                      enabled: draft.enabled,
                      globalOverridePath: draft.globalOverridePath,
                      projectOverridePath: draft.projectOverridePath,
                    })
                  }
                  isDisabled={save.isPending}
                >
                  Save {descriptor.displayName}
                </PrimaryButton>
              </article>
            );
          })}
        </div>
      )}
      {save.error ? <ErrorBanner error={save.error} /> : null}
    </section>
  );
}

function WorkspaceSection({
  roots,
  projects,
  loading,
  error,
}: {
  roots: WorkspaceRootView[];
  projects: Awaited<ReturnType<typeof api.manualProjectsList>>;
  loading: boolean;
  error: unknown;
}) {
  const client = useQueryClient();
  const [path, setPath] = useState('');
  const [depth, setDepth] = useState(8);
  const [ignores, setIgnores] = useState('');
  const [projectPath, setProjectPath] = useState('');
  const invalidate = () => invalidateWorkspace(client);
  const mutate = useMutation({
    mutationFn: async (action: { kind: string; root?: WorkspaceRootView; path?: string }) => {
      if (action.kind === 'add')
        return api.workspaceRootAdd({
          selectedPath: path,
          maximumDepth: depth,
          ignoreRules: lines(ignores),
        });
      if (action.kind === 'project') return api.manualProjectAdd({ selectedPath: projectPath });
      if (action.kind === 'pause')
        return api.workspaceRootPause({
          rootId: action.root!.rootId,
          paused: !action.root!.paused,
        });
      if (action.kind === 'remove') return api.workspaceRootRemove(action.root!.rootId);
      if (action.kind === 'rescan') return api.workspaceRootRescan(action.root!.rootId);
      return api.manualProjectRescan(action.path!);
    },
    onSuccess: invalidate,
  });
  return (
    <section className={styles.section} aria-labelledby="workspace-heading">
      <div className={styles.sectionHeading}>
        <div>
          <h2 id="workspace-heading">Workspace Roots</h2>
          <p>Read-only project discovery with explicit coverage evidence.</p>
        </div>
      </div>
      <div className={styles.formRow}>
        <TextField value={path} onChange={setPath}>
          <Label>Root path</Label>
          <Input />
        </TextField>
        <TextField value={String(depth)} onChange={(v) => setDepth(Number(v))}>
          <Label>Maximum depth (1–32)</Label>
          <Input type="number" min={1} max={32} />
        </TextField>
        <TextField value={ignores} onChange={setIgnores}>
          <Label>Ignore rules (one per line)</Label>
          <TextArea />
        </TextField>
        <PrimaryButton
          onPress={() => mutate.mutate({ kind: 'add' })}
          isDisabled={!path || depth < 1 || depth > 32 || mutate.isPending}
        >
          Add root
        </PrimaryButton>
      </div>
      {loading ? (
        <LoadingBlock label="Loading Workspace Roots" />
      ) : error ? (
        <ErrorBanner error={error} />
      ) : (
        roots.map((root) => (
          <WorkspaceRoot
            key={root.rootId}
            root={root}
            pending={mutate.isPending}
            run={(kind) => mutate.mutate({ kind, root })}
          />
        ))
      )}
      <h3>Manual projects</h3>
      <div className={styles.formRow}>
        <TextField value={projectPath} onChange={setProjectPath}>
          <Label>Project path</Label>
          <Input />
        </TextField>
        <PrimaryButton
          onPress={() => mutate.mutate({ kind: 'project' })}
          isDisabled={!projectPath || mutate.isPending}
        >
          Add project
        </PrimaryButton>
      </div>
      {projects.map((project) => (
        <div className={styles.listRow} key={project.projectId}>
          <span>
            <PathText path={project.rootPath} /> · {project.git ? 'Git' : 'non-Git'}
          </span>
          <SecondaryButton
            onPress={() => mutate.mutate({ kind: 'project-rescan', path: project.projectId })}
            isDisabled={mutate.isPending}
          >
            Rescan
          </SecondaryButton>
        </div>
      ))}
      {mutate.error ? <ErrorBanner error={mutate.error} /> : null}
    </section>
  );
}

function WorkspaceRoot({
  root,
  pending,
  run,
}: {
  root: WorkspaceRootView;
  pending: boolean;
  run: (kind: string) => void;
}) {
  const client = useQueryClient();
  const [selectedPath, setSelectedPath] = useState(root.selectedPath);
  const [depth, setDepth] = useState(root.maximumDepth);
  const [ignores, setIgnores] = useState(root.ignoreRules.join('\n'));
  const update = useMutation({
    mutationFn: () =>
      api.workspaceRootUpdate({
        rootId: root.rootId,
        selectedPath: selectedPath === root.selectedPath ? null : selectedPath,
        maximumDepth: depth,
        ignoreRules: lines(ignores),
      }),
    onSuccess: async () => {
      await invalidateWorkspace(client);
    },
  });
  const state = root.coverageState.toLowerCase();
  const tone = state === 'complete' ? 'success' : state === 'stale' ? 'pending' : 'danger';
  return (
    <article className={styles.root}>
      <div className={styles.cardTitle}>
        <PathText path={root.selectedPath} />
        <StatusPill tone={tone}>{root.coverageState}</StatusPill>
      </div>
      <p>
        {root.projectCount} projects · {root.skillCount} skills · {root.errorCount} errors · Last
        attempt {date(root.lastAttempt)} · Last complete {date(root.lastSuccessfulCompleteScan)}
      </p>
      <p>
        {root.noFilesChanged
          ? 'No files changed in the last scan.'
          : 'The last scan observed filesystem changes.'}
      </p>
      {root.errors.map((item) => (
        <div className={styles.diagnostic} key={`${item.code}:${item.path}`}>
          <b>{item.code}</b>: {item.summary}
          <PathText path={item.path} />
        </div>
      ))}
      <div className={styles.formRow}>
        <TextField value={selectedPath} onChange={setSelectedPath}>
          <Label>Root path</Label>
          <Input />
        </TextField>
        <TextField value={String(depth)} onChange={(v) => setDepth(Number(v))}>
          <Label>Depth</Label>
          <Input type="number" min={1} max={32} />
        </TextField>
        <TextField value={ignores} onChange={setIgnores}>
          <Label>Ignore rules</Label>
          <TextArea />
        </TextField>
        <PrimaryButton
          onPress={() => update.mutate()}
          isDisabled={!selectedPath || depth < 1 || depth > 32 || pending || update.isPending}
        >
          Save root
        </PrimaryButton>
      </div>
      <div className={styles.actions}>
        <SecondaryButton onPress={() => run('pause')} isDisabled={pending}>
          {root.paused ? 'Resume' : 'Pause'}
        </SecondaryButton>
        <SecondaryButton onPress={() => run('rescan')} isDisabled={pending}>
          Rescan
        </SecondaryButton>
        <DangerButton onPress={() => run('remove')} isDisabled={pending}>
          Remove
        </DangerButton>
      </div>
      {update.error ? <ErrorBanner error={update.error} /> : null}
    </article>
  );
}

function invalidateWorkspace(client: QueryClient) {
  return Promise.all([
    client.invalidateQueries({ queryKey: queryKeys.workspaceRoots }),
    client.invalidateQueries({ queryKey: queryKeys.manualProjects }),
    client.invalidateQueries({ queryKey: ['library'] }),
    client.invalidateQueries({ queryKey: ['skill'] }),
    client.invalidateQueries({ queryKey: ['activity'] }),
  ]);
}

function TargetsSection({
  targets,
  projects,
  loading,
  error,
}: {
  targets: Awaited<ReturnType<typeof api.targetsList>>;
  projects: Awaited<ReturnType<typeof api.manualProjectsList>>;
  loading: boolean;
  error: unknown;
}) {
  const client = useQueryClient();
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [scope, setScope] = useState<CustomTargetScope>('global');
  const [mode, setMode] = useState<DeploymentModeDto>('symlink');
  const [projectId, setProjectId] = useState('');
  const register = useMutation({
    mutationFn: () =>
      api.customTargetRegister({
        targetId: null,
        displayName: name,
        selectedDirectory: path,
        scope,
        preferredMode: mode,
        projectId: scope === 'project' ? projectId : null,
      }),
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: queryKeys.targets });
    },
  });
  return (
    <section className={styles.section} aria-labelledby="targets-heading">
      <div className={styles.sectionHeading}>
        <div>
          <h2 id="targets-heading">Custom targets</h2>
          <p>Concrete destinations using the same deployment safeguards as built-in targets.</p>
        </div>
      </div>
      <div className={styles.formRow}>
        <TextField value={name} onChange={setName}>
          <Label>Display name</Label>
          <Input />
        </TextField>
        <TextField value={path} onChange={setPath}>
          <Label>Directory</Label>
          <Input />
        </TextField>
        <label>
          Scope
          <select value={scope} onChange={(e) => setScope(e.target.value as CustomTargetScope)}>
            <option value="global">Global</option>
            <option value="project">Project</option>
          </select>
        </label>
        <label>
          Preferred mode
          <select value={mode} onChange={(e) => setMode(e.target.value as DeploymentModeDto)}>
            <option value="symlink">Symlink</option>
            <option value="managed_copy">Managed copy</option>
          </select>
        </label>
        {scope === 'project' ? (
          <label>
            Project
            <select value={projectId} onChange={(e) => setProjectId(e.target.value)}>
              <option value="">Select project</option>
              {projects.map((p) => (
                <option value={p.projectId} key={p.projectId}>
                  {p.rootPath}
                </option>
              ))}
            </select>
          </label>
        ) : null}
        <PrimaryButton
          onPress={() => register.mutate()}
          isDisabled={!name || !path || (scope === 'project' && !projectId) || register.isPending}
        >
          Register target
        </PrimaryButton>
      </div>
      {loading ? (
        <LoadingBlock label="Loading targets" />
      ) : error ? (
        <ErrorBanner error={error} />
      ) : (
        targets
          .filter((target) => target.isCustom)
          .map((target) => (
            <div className={styles.listRow} key={target.targetId}>
              <span>
                <b>{target.targetId}</b> · {target.scope} · {target.defaultMode}
                <PathText path={target.rootPath} />
              </span>
            </div>
          ))
      )}
      {register.error ? <ErrorBanner error={register.error} /> : null}
    </section>
  );
}

export default SettingsPanel;
