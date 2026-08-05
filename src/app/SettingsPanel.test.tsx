import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { api } from '../lib/api';
import { SettingsPanel } from './SettingsPanel';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: new Proxy(
      {},
      {
        get: (target, key) =>
          (target as Record<PropertyKey, unknown>)[key] ??
          ((target as Record<PropertyKey, unknown>)[key] = vi.fn()),
      },
    ),
  };
});

function renderPanel() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(<SettingsPanel />, {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  });
}

describe('SettingsPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.vaultStatus).mockResolvedValue({
      initialized: true,
      rootPath: '/vault',
      defaultPath: '/default',
      startupRecoveryCompleted: true,
    });
    vi.mocked(api.adaptersList).mockResolvedValue([
      {
        adapterId: 'codex',
        displayName: 'Codex',
        platform: 'macos',
        globalPath: '.codex/skills',
        projectPath: '.codex/skills',
        scopes: ['global', 'project'],
        supportedModes: ['symlink'],
        officialSourceUrl: 'https://example.test',
        verifiedAt: '2026-08-01T00:00:00Z',
        confidence: 'Verified',
        caveats: 'Project discovery requires a repository.',
      },
    ]);
    vi.mocked(api.adaptersConfiguredList).mockResolvedValue([
      {
        adapterId: 'codex',
        displayName: 'Codex',
        enabled: true,
        globalOverridePath: null,
        projectOverridePath: null,
      },
    ]);
    vi.mocked(api.workspaceRootsList).mockResolvedValue([]);
    vi.mocked(api.manualProjectsList).mockResolvedValue([]);
    vi.mocked(api.targetsList).mockResolvedValue([]);
    vi.mocked(api.trashRetentionSummary).mockResolvedValue({
      totalEntries: 0,
      expiredEntries: 0,
      protectedEntries: 0,
      nextDeadline: null,
    });
    vi.mocked(api.vaultObjectGcSettings).mockResolvedValue({
      retentionDays: 30,
      lastRun: null,
      eligible: true,
      nextRun: null,
      disabledReasons: [],
    });
  });

  it('shows incomplete coverage diagnostics without describing it as complete', async () => {
    vi.mocked(api.workspaceRootsList).mockResolvedValue([
      {
        rootId: 'root-1',
        selectedPath: '/workspace/very/long/path',
        canonicalPath: '/workspace/very/long/path',
        enabled: true,
        paused: false,
        maximumDepth: 8,
        ignoreRules: ['node_modules'],
        coverageState: 'incomplete',
        lastAttempt: '2026-08-05T10:00:00Z',
        lastSuccessfulCompleteScan: null,
        projectCount: 2,
        skillCount: 3,
        errorCount: 1,
        errors: [
          {
            path: '/workspace/private',
            code: 'permission_denied',
            summary: 'Directory could not be read',
          },
        ],
        noFilesChanged: true,
      },
    ]);
    renderPanel();
    expect(await screen.findByText('incomplete')).toBeInTheDocument();
    expect(screen.getByText('permission_denied')).toBeInTheDocument();
    expect(screen.getByText(/Directory could not be read/)).toBeInTheDocument();
    expect(screen.getByText('No files changed in the last scan.')).toBeInTheDocument();
    expect(screen.getByText(/Last complete Never/)).toBeInTheDocument();
  });

  it('keeps adapter changes as drafts until explicit Save', async () => {
    const user = userEvent.setup();
    vi.mocked(api.adapterConfigure).mockImplementation(async (request) => ({
      ...request,
      displayName: 'Codex',
    }));
    renderPanel();
    const enabled = await screen.findByRole('checkbox', { name: 'Enabled' });
    expect(enabled).toBeChecked();
    await user.click(enabled);
    expect(enabled).not.toBeChecked();
    expect(api.adapterConfigure).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Save Codex' }));
    await waitFor(() =>
      expect(api.adapterConfigure).toHaveBeenCalledWith({
        adapterId: 'codex',
        enabled: false,
        globalOverridePath: null,
        projectOverridePath: null,
      }),
    );
  });

  it('requires repair plan review before a separate execute action', async () => {
    const user = userEvent.setup();
    vi.mocked(api.vaultRepairPlan).mockResolvedValue({
      operationId: 'op-14',
      planDigest: 'digest-14',
      writable: true,
      actions: [
        {
          kind: 'rebuild_row',
          exactPath: '/vault/index.sqlite',
          reason: 'Missing derived row',
          requiresReviewedOperation: true,
        },
      ],
      refused: [],
      executionAllowed: true,
      disabledReasons: [],
    });
    vi.mocked(api.vaultRepairExecute).mockResolvedValue(1);
    renderPanel();
    await user.click(await screen.findByRole('button', { name: 'Review repair' }));
    expect(await screen.findByLabelText('repair plan review')).toHaveTextContent('op-14');
    expect(api.vaultRepairExecute).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Execute reviewed plan' }));
    await waitFor(() =>
      expect(api.vaultRepairExecute).toHaveBeenCalledWith({
        operationId: 'op-14',
        planDigest: 'digest-14',
      }),
    );
    expect(await screen.findByText('1 repairs applied')).toBeInTheDocument();
  });
});
