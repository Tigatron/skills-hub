import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { BootstrapState } from '../bindings';
import { getBootstrapState } from '../lib/bootstrap';
import { App } from './App';

vi.mock('../lib/bootstrap', () => ({
  getBootstrapState: vi.fn(),
}));

const bootstrapState: BootstrapState = {
  appName: 'Skills Hub',
  appVersion: '0.1.0',
  bundleIdentifier: 'com.terrylan.skillshub',
  contractVersion: 1,
  implementationStage: 'M0-001',
  runtimeStatus: 'ready',
  blockingWorkerLimit: 4,
  platform: {
    os: 'macos',
    arch: 'aarch64',
    minimumSupportedOs: 'macOS 14 Sonoma',
  },
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

describe('App', () => {
  beforeEach(() => {
    vi.mocked(getBootstrapState).mockReset();
  });

  it('renders authoritative bootstrap state returned by Rust', async () => {
    vi.mocked(getBootstrapState).mockResolvedValue(bootstrapState);

    renderApp();

    expect(screen.getByText('No filesystem access')).toBeInTheDocument();
    expect(await screen.findByText('Runtime readiness')).toBeInTheDocument();
    expect(await screen.findByText('Connected')).toBeInTheDocument();
    expect(screen.getByText('macos · aarch64')).toBeInTheDocument();
    expect(screen.getByText('4 concurrent jobs maximum')).toBeInTheDocument();
    expect(screen.getByText('M0-002 · Identity, hashing, and path contracts')).toBeInTheDocument();
  });

  it('keeps a failed backend connection explicit and retryable', async () => {
    const user = userEvent.setup();
    vi.mocked(getBootstrapState)
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(bootstrapState);

    renderApp();

    expect(await screen.findByText('Rust backend unavailable')).toBeInTheDocument();
    expect(screen.getByText(/No files were changed\./)).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'Retry connection' }));

    expect(await screen.findByText('Connected')).toBeInTheDocument();
  });
});
