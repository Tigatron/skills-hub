import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { BootstrapState, LibraryPage, VaultStatusView } from '../bindings';
import { api } from '../lib/api';
import { App } from './App';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      bootstrapGetState: vi.fn(),
      vaultStatus: vi.fn(),
      vaultInitialize: vi.fn(),
      libraryList: vi.fn(),
      scanStart: vi.fn(),
      scanGet: vi.fn(),
      targetsList: vi.fn(),
      deploymentsList: vi.fn(),
      activityList: vi.fn(),
      activityDetail: vi.fn(),
    },
    listenDomainInvalidated: vi.fn(async () => () => undefined),
    listenScanProgress: vi.fn(async () => () => undefined),
  };
});

const bootstrapState: BootstrapState = {
  appName: 'Skills Hub',
  appVersion: '0.1.0',
  bundleIdentifier: 'com.terrylan.skillshub',
  contractVersion: 1,
  implementationStage: 'M0-009',
  vaultInitialized: false,
  vaultPath: null,
  runtimeStatus: 'ready',
  blockingWorkerLimit: 4,
  platform: {
    os: 'macos',
    arch: 'aarch64',
    minimumSupportedOs: 'macOS 14 Sonoma',
  },
};

const vaultStatus: VaultStatusView = {
  initialized: false,
  rootPath: null,
  defaultPath: '/tmp/Skills Hub/Vault',
  startupRecoveryCompleted: null,
};

const emptyLibrary: LibraryPage = {
  items: [],
  total: 0,
  offset: 0,
  limit: 100,
};

function renderApp() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  });

  return render(<App />, {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    ),
  });
}

describe('App thin slice', () => {
  beforeEach(() => {
    vi.mocked(api.bootstrapGetState).mockReset();
    vi.mocked(api.vaultStatus).mockReset();
    vi.mocked(api.vaultInitialize).mockReset();
    vi.mocked(api.libraryList).mockReset();
    vi.mocked(api.targetsList).mockReset();
    vi.mocked(api.deploymentsList).mockReset();
    vi.mocked(api.activityList).mockReset();
  });

  it('shows first-run Vault initialization before filesystem workflows', async () => {
    vi.mocked(api.bootstrapGetState).mockResolvedValue(bootstrapState);
    vi.mocked(api.vaultStatus).mockResolvedValue(vaultStatus);

    renderApp();

    expect(await screen.findByRole('heading', { name: 'Create a Vault' })).toBeInTheDocument();
    expect(screen.getByText(/Default path/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Initialize default Vault' })).toBeInTheDocument();
  });

  it('renders Library after the Vault is ready and keeps navigation keyboard reachable', async () => {
    const user = userEvent.setup();
    vi.mocked(api.bootstrapGetState).mockResolvedValue({
      ...bootstrapState,
      vaultInitialized: true,
      vaultPath: '/tmp/Skills Hub/Vault',
    });
    vi.mocked(api.vaultStatus).mockResolvedValue({
      ...vaultStatus,
      initialized: true,
      rootPath: '/tmp/Skills Hub/Vault',
      startupRecoveryCompleted: true,
    });
    vi.mocked(api.libraryList).mockResolvedValue(emptyLibrary);
    vi.mocked(api.targetsList).mockResolvedValue([]);
    vi.mocked(api.deploymentsList).mockResolvedValue({ items: [], count: 0 });
    vi.mocked(api.activityList).mockResolvedValue([]);

    renderApp();

    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument();
    expect(await screen.findByText(/No Skills observed yet/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Scan Universal global' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Activity' }));
    expect(await screen.findByRole('heading', { name: 'Activity' })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Deployments' }));
    expect(await screen.findByRole('heading', { name: 'Deployments' })).toBeInTheDocument();
  });

  it('initializes the default Vault through the real command wrapper', async () => {
    const user = userEvent.setup();
    vi.mocked(api.bootstrapGetState)
      .mockResolvedValueOnce(bootstrapState)
      .mockResolvedValue({
        ...bootstrapState,
        vaultInitialized: true,
        vaultPath: '/tmp/Skills Hub/Vault',
      });
    vi.mocked(api.vaultStatus)
      .mockResolvedValueOnce(vaultStatus)
      .mockResolvedValue({
        ...vaultStatus,
        initialized: true,
        rootPath: '/tmp/Skills Hub/Vault',
        startupRecoveryCompleted: true,
      });
    vi.mocked(api.vaultInitialize).mockResolvedValue({
      rootPath: '/tmp/Skills Hub/Vault',
      initialized: true,
      vaultId: 'vault-1',
    });
    vi.mocked(api.libraryList).mockResolvedValue(emptyLibrary);
    vi.mocked(api.targetsList).mockResolvedValue([]);

    renderApp();

    await user.click(await screen.findByRole('button', { name: 'Initialize default Vault' }));

    await waitFor(() => {
      expect(api.vaultInitialize).toHaveBeenCalledWith({ selectedDirectory: null });
    });
    expect(await screen.findByRole('heading', { name: 'Library' })).toBeInTheDocument();
  });
});
