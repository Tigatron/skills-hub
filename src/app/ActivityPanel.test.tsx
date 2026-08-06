import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { api } from '../lib/api';
import { ActivityPanel } from './ActivityPanel';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return { ...actual, api: { activityList: vi.fn(), activityDetail: vi.fn() } };
});

describe('ActivityPanel', () => {
  it('renders the persisted empty state', async () => {
    vi.mocked(api.activityList).mockResolvedValue([]);
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(<ActivityPanel />, {
      wrapper: ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={client}>{children}</QueryClientProvider>
      ),
    });

    expect(await screen.findByRole('heading', { name: 'No activity yet' })).toBeInTheDocument();
    expect(screen.getByText(/Scans and Operations project here/)).toBeInTheDocument();
  });

  it('persists recovery-required evidence and copyable recovery references', async () => {
    const item = {
      id: 'activity-1',
      kind: 'deploy',
      state: 'completed',
      outcome: 'recovery_required',
      summary: 'Deployment needs recovery',
      startedAt: '2026-08-05T00:00:00Z',
      completedAt: '2026-08-05T00:01:00Z',
      operationId: 'operation-1',
      scanRunId: null,
      tone: 'danger' as const,
    };
    vi.mocked(api.activityList).mockResolvedValue([item]);
    vi.mocked(api.activityDetail).mockResolvedValue({
      item,
      detailsJson: '{}',
      operation: {
        recoveryAvailable: true,
        errorCode: 'recovery_required',
        failedStep: 2,
        planReference: '.manager/operations/operation-1/plan.json',
        journalReference: '.manager/operations/operation-1/journal.json',
        recoveryReferences: ['snapshot:one'],
        paths: [],
      },
      scan: null,
    });
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(<ActivityPanel />, {
      wrapper: ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={client}>{children}</QueryClientProvider>
      ),
    });

    await userEvent.click(await screen.findByRole('option', { name: /Deployment needs recovery/ }));
    expect((await screen.findAllByText('recovery_required')).length).toBeGreaterThan(1);
    const activityRow = screen.getByRole('option', { name: /Deployment needs recovery/ });
    expect(activityRow.querySelector('[data-tone]')).toHaveAttribute('data-tone', 'danger');
    expect(screen.getByRole('button', { name: 'Copy snapshot:one' })).toBeInTheDocument();
  });
});
