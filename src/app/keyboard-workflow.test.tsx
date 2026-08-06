import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type {
  AnyOperationView,
  DeploymentHealthView,
  DeploymentPlanView,
  LibraryItem,
  LibraryPage,
  SkillDetail,
  TakeoverPlanView,
  TargetView,
  TrashEntryView,
  TrashPlanView,
} from '../bindings';
import { api } from '../lib/api';
import { DeploymentsPanel } from './DeploymentsPanel';
import { LibraryPanel } from './LibraryPanel';
import { TrashPanel } from './TrashPanel';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      ...actual.api,
      libraryList: vi.fn(),
      targetsList: vi.fn(),
      trashEntriesList: vi.fn(),
      trashRetentionSummary: vi.fn(),
      scanStart: vi.fn(),
      scanGet: vi.fn(),
      takeoverPlan: vi.fn(),
      operationExecute: vi.fn(),
      operationCancel: vi.fn(),
      operationPlanExport: vi.fn(),
      skillGet: vi.fn(),
      deploymentsList: vi.fn(),
      deploymentPlan: vi.fn(),
      deploymentVerify: vi.fn(),
      undeployPlan: vi.fn(),
      trashMovePlan: vi.fn(),
      trashRestorePlan: vi.fn(),
      trashExecute: vi.fn(),
    },
  };
});

vi.mock('@tanstack/react-virtual', () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 92,
    getVirtualItems: () =>
      Array.from({ length: Math.min(count, 12) }, (_, index) => ({
        index,
        key: index,
        start: index * 92,
        size: 92,
      })),
    measureElement: () => undefined,
  }),
}));

const externalItem: LibraryItem = {
  id: 'item-external',
  skillId: null,
  displayName: 'keyboard-skill',
  deploymentName: 'keyboard-skill',
  ownership: 'external',
  sourceSummary: 'Universal global',
  locations: [
    {
      observationId: 'observation-1',
      adapterId: 'universal',
      sourceRootId: 'global',
      path: '/tmp/keyboard-skill',
      status: 'observed',
      error: null,
    },
  ],
  digest: 'digest-1',
  validation: 'verified',
  duplicateSummary: {
    exactDuplicateLocations: 0,
    nameConflicts: 0,
    probableDuplicatesOrRenames: 0,
    unverified: false,
  },
  deploymentCount: 0,
  workingLocation: null,
  changedAt: '2026-08-06T00:00:00Z',
  nextActions: ['keep_external', 'add_to_vault', 'add_and_manage'],
};

const vaultedItem: LibraryItem = {
  ...externalItem,
  id: 'item-vaulted',
  skillId: 'skill-1',
  ownership: 'vaulted',
  workingLocation: 'skills/skill-1/keyboard-skill',
  nextActions: [],
};

const skillDetail: SkillDetail = {
  skillId: 'skill-1',
  displayName: 'keyboard-skill',
  deploymentName: 'keyboard-skill',
  workingPath: 'skills/skill-1/keyboard-skill',
  workingDigest: 'digest-1',
  baselineDigest: 'digest-1',
  ownership: 'vaulted',
  lifecycle: 'active',
  sourcePaths: ['/tmp/keyboard-skill'],
  deploymentPaths: [],
  observationPaths: [],
  conflicts: [],
  snapshot: {
    available: false,
    count: 0,
    latestCreatedAt: null,
    protectedCount: 0,
    unavailableReason: 'No snapshots',
  },
  activity: [],
  capabilities: [
    { action: 'deploy', allowed: true, disabledReason: null },
    { action: 'move_to_trash', allowed: true, disabledReason: null },
    { action: 'preview', allowed: true, disabledReason: null },
    { action: 'reveal', allowed: true, disabledReason: null },
  ],
};

const takeoverPlan: TakeoverPlanView = {
  operationId: 'op-takeover',
  planDigest: 'digest-takeover',
  expiresAt: '2026-08-06T12:00:00Z',
  decision: 'add_to_vault',
  skillId: 'skill-1',
  observations: [],
  reviewedDigest: 'digest-1',
  workingPath: 'skills/skill-1/keyboard-skill',
  baselineObjectPath: '.manager/objects/digest-1',
  manifestPath: '.manager/manifests/skills/skill-1.json',
  selectedReplacements: [],
  entryCount: 1,
  byteCount: 32,
  blockers: [],
  recoverySummary: 'Rollback restores the prior external observation only.',
  recoveryCount: 1,
  crossVolumeConsequence: null,
  executionAllowed: true,
};

const deployPlan: DeploymentPlanView = {
  operationId: 'op-deploy',
  planDigest: 'digest-deploy',
  expiresAt: '2026-08-06T12:00:00Z',
  action: 'deploy',
  skillId: 'skill-1',
  targetId: 'target-1',
  deploymentId: 'deployment-1',
  targetPath: '/tmp/target/keyboard-skill',
  requestedMode: 'symlink',
  resolvedMode: 'symlink',
  fallbackReason: null,
  reviewedHealth: 'missing_target',
  noOp: false,
  consequence: 'Create a managed symlink.',
  recoveryCount: 1,
  executionAllowed: true,
};

const deployOperation: AnyOperationView = {
  kind: 'deployment',
  value: {
    operationId: 'op-deploy',
    planDigest: 'digest-deploy',
    state: 'finalized',
    terminal: true,
    tone: 'success',
    outcome: 'succeeded',
    failure: null,
    recovery: [],
    replayed: false,
    cancellationAllowed: false,
    review: deployPlan,
  },
};

const deploymentRow: DeploymentHealthView = {
  deploymentId: 'deployment-1',
  skillId: 'skill-1',
  targetId: 'target-1',
  deploymentName: 'keyboard-skill',
  targetPath: '/tmp/target/keyboard-skill',
  mode: 'symlink',
  active: true,
  health: 'clean',
  explanation: 'Matches expected symlink target.',
  expectedDigest: 'digest-1',
  vaultDigest: 'digest-1',
  targetDigest: 'digest-1',
  expectedLinkTarget: '/vault/skills/skill-1/keyboard-skill',
  actualLinkTarget: '/vault/skills/skill-1/keyboard-skill',
  driftDirection: 'none',
  allowedActions: ['verify', 'undeploy', 'redeploy'],
  disabledReason: null,
  verifiedAt: '2026-08-06T00:00:00Z',
};

const target: TargetView = {
  targetId: 'target-1',
  adapterId: 'universal-agent-skills@1',
  scope: 'global',
  projectId: null,
  projectKind: null,
  rootPath: '/tmp/target',
  isOverride: false,
  isCustom: true,
  defaultMode: 'symlink',
};

const trashEntry: TrashEntryView = {
  entryId: 'entry-1',
  skillId: 'skill-1',
  displayName: 'keyboard-skill',
  originalWorkingPath: 'skills/skill-1/keyboard-skill',
  trashedAt: '2026-08-06T00:00:00Z',
  retentionDeadline: null,
  retentionPolicy: 'never',
  protectedReferences: [],
};

const trashPlan: TrashPlanView = {
  operationId: 'op-trash',
  planDigest: 'digest-trash',
  entry: trashEntry,
  blockers: [],
  executionAllowed: true,
};

function page(items: LibraryItem[]): LibraryPage {
  return { items, total: items.length, offset: 0, limit: 100 };
}

function wrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

describe('keyboard core workflow', () => {
  beforeEach(() => {
    vi.mocked(api.libraryList).mockResolvedValue(page([externalItem]));
    vi.mocked(api.targetsList).mockResolvedValue([target]);
    vi.mocked(api.trashEntriesList).mockResolvedValue([]);
    vi.mocked(api.trashRetentionSummary).mockResolvedValue({
      totalEntries: 0,
      expiredEntries: 0,
      protectedEntries: 0,
      nextDeadline: null,
    });
    vi.mocked(api.scanStart).mockResolvedValue({ jobId: 'scan-1' });
    vi.mocked(api.scanGet).mockResolvedValue({
      jobId: 'scan-1',
      adapterId: 'universal-agent-skills@1',
      sourceRootId: 'universal',
      sourceName: 'Universal',
      displayRoot: '/tmp/home/.agents/skills',
      state: 'completed',
      coverage: { state: 'complete', complete: true, noFilesChanged: true },
      completedEntries: 1,
      estimatedEntries: 1,
      observationCount: 1,
      errorCount: 0,
      errors: [],
      startedAt: '2026-08-06T00:00:00Z',
      completedAt: '2026-08-06T00:00:01Z',
    });
    vi.mocked(api.takeoverPlan).mockResolvedValue(takeoverPlan);
    vi.mocked(api.operationExecute).mockResolvedValue(deployOperation);
    vi.mocked(api.operationPlanExport).mockResolvedValue({
      operationId: 'op-trash',
      planDigest: 'digest-trash',
      json: '{"action":"restore"}',
    });
    vi.mocked(api.skillGet).mockResolvedValue(skillDetail);
    vi.mocked(api.deploymentsList).mockResolvedValue({ items: [deploymentRow], count: 1 });
    vi.mocked(api.deploymentPlan).mockResolvedValue(deployPlan);
    vi.mocked(api.deploymentVerify).mockResolvedValue(deploymentRow);
    vi.mocked(api.undeployPlan).mockResolvedValue({
      ...deployPlan,
      operationId: 'op-undeploy',
      planDigest: 'digest-undeploy',
      action: 'undeploy',
    });
    vi.mocked(api.trashMovePlan).mockResolvedValue(trashPlan);
    vi.mocked(api.trashRestorePlan).mockResolvedValue({
      ...trashPlan,
      operationId: 'op-restore',
      planDigest: 'digest-restore',
    });
    vi.mocked(api.trashExecute).mockResolvedValue({
      operationId: 'op-restore',
      outcome: 'succeeded',
      succeeded: true,
      tone: 'success',
      replayed: false,
    });
  });

  it('supports the scan → takeover → plan → deploy keyboard path with focusable controls', async () => {
    const user = userEvent.setup();
    render(<LibraryPanel />, { wrapper });

    const scan = await screen.findByRole('button', { name: 'Scan Universal global' });
    scan.focus();
    expect(scan).toHaveFocus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(api.scanStart).toHaveBeenCalled());

    const option = await screen.findByRole('option', { name: /keyboard-skill/i });
    option.focus();
    expect(option).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(option).toHaveAttribute('aria-selected', 'true');

    const planAdd = await screen.findByRole('button', { name: 'Plan Add to Vault' });
    planAdd.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(api.takeoverPlan).toHaveBeenCalled());
    expect(await screen.findByRole('heading', { name: 'Takeover plan' })).toBeInTheDocument();

    const execute = screen.getByRole('button', { name: 'Execute reviewed plan' });
    execute.focus();
    expect(execute).toHaveFocus();

    // Dismiss takeover plan and continue with deploy using a Vaulted Skill ID.
    await user.click(screen.getByRole('button', { name: 'Dismiss' }));
    vi.mocked(api.libraryList).mockResolvedValue(page([vaultedItem]));
    const skillIdField = screen.getByRole('textbox', { name: 'Vaulted Skill ID' });
    await user.clear(skillIdField);
    await user.type(skillIdField, 'skill-1');
    await user.selectOptions(
      screen.getByRole('combobox', { name: 'Deployment target' }),
      'target-1',
    );
    const deploy = screen.getByRole('button', { name: 'Plan deploy' });
    expect(deploy).toBeEnabled();
    deploy.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(api.deploymentPlan).toHaveBeenCalled());
    expect(await screen.findByRole('heading', { name: 'Deployment plan' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Dismiss' }));
    expect(screen.queryByRole('heading', { name: 'Deployment plan' })).not.toBeInTheDocument();
  });

  it('supports verify and undeploy planning from Deployments via keyboard', async () => {
    const user = userEvent.setup();
    render(<DeploymentsPanel />, { wrapper });

    const select = await screen.findByRole('button', { name: 'Select keyboard-skill' });
    select.focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('heading', { name: 'keyboard-skill' })).toBeInTheDocument();

    const verify = screen.getByRole('button', { name: 'Verify selected' });
    verify.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(api.deploymentVerify).toHaveBeenCalled());

    const undeploy = screen.getByRole('button', { name: 'Plan clean undeploy' });
    undeploy.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(api.undeployPlan).toHaveBeenCalled());
    expect(await screen.findByRole('heading', { name: 'Deployment plan' })).toBeInTheDocument();
  });

  it('supports Trash restore planning by keyboard', async () => {
    const user = userEvent.setup();
    vi.mocked(api.trashEntriesList).mockResolvedValue([trashEntry]);
    vi.mocked(api.trashRetentionSummary).mockResolvedValue({
      totalEntries: 1,
      expiredEntries: 0,
      protectedEntries: 0,
      nextDeadline: null,
    });
    render(<TrashPanel />, { wrapper });

    const option = await screen.findByRole('option', { name: /keyboard-skill/i });
    option.focus();
    await user.keyboard('{Enter}');
    const restore = await screen.findByRole('button', { name: 'Plan restore' });
    restore.focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(api.trashRestorePlan).toHaveBeenCalled());
    expect(await screen.findByRole('heading', { name: 'Restore plan' })).toBeInTheDocument();
  });
});
