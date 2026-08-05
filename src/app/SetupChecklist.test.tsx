import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { api } from '../lib/api';
import { SetupChecklist } from './SetupChecklist';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      workspaceRootsList: vi.fn(),
      targetsList: vi.fn(),
      deploymentsList: vi.fn(),
    },
  };
});

function renderChecklist() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<SetupChecklist />, {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  });
}

describe('SetupChecklist', () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: {
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => values.set(key, value),
        clear: () => values.clear(),
      },
    });
    vi.mocked(api.workspaceRootsList).mockResolvedValue([]);
    vi.mocked(api.targetsList).mockResolvedValue([]);
    vi.mocked(api.deploymentsList).mockResolvedValue({ items: [], count: 0 });
  });

  it('renders backend-derived progress and persists dismiss/show', async () => {
    const user = userEvent.setup();
    renderChecklist();
    expect(await screen.findByText('Setup 1/4')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Hide' }));
    expect(window.localStorage.getItem('skills-hub.m0-setup-checklist-dismissed')).toBe('true');
    await user.click(screen.getByRole('button', { name: 'Show checklist' }));
    expect(await screen.findByText('Setup 1/4')).toBeInTheDocument();
    expect(window.localStorage.getItem('skills-hub.m0-setup-checklist-dismissed')).toBe('false');
  });
});
