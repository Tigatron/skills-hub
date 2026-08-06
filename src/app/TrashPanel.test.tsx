import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { TrashEntryView, TrashPlanView } from '../bindings';
import { api } from '../lib/api';
import { TrashPanel } from './TrashPanel';

const entry: TrashEntryView = {
  entryId: 'trash-1',
  skillId: 'skill-1',
  displayName: 'Example Skill',
  originalWorkingPath: '/Users/example/.skills/very/long/path/example-skill',
  trashedAt: '2026-08-01T12:00:00Z',
  retentionDeadline: '2026-09-01T12:00:00Z',
  retentionPolicy: '30 days',
  protectedReferences: ['activity:takeover-1'],
};

const reviewedPlan: TrashPlanView = {
  operationId: 'operation-1',
  planDigest: 'digest-1',
  entry,
  blockers: [],
  executionAllowed: true,
};

afterEach(() => vi.restoreAllMocks());

function arrange(entries: TrashEntryView[] = [entry]) {
  vi.spyOn(api, 'operationPlanExport').mockResolvedValue({
    operationId: 'operation-1',
    planDigest: 'digest-1',
    json: '{\n  "operationId": "operation-1",\n  "destinationRelativePath": "skills/new"\n}',
  });
  vi.spyOn(api, 'trashEntriesList').mockResolvedValue(entries);
  vi.spyOn(api, 'trashRetentionSummary').mockResolvedValue({
    totalEntries: entries.length,
    expiredEntries: 0,
    protectedEntries: entries.filter((item) => item.protectedReferences.length > 0).length,
    nextDeadline: entries[0]?.retentionDeadline ?? null,
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    client,
    ...render(
      <QueryClientProvider client={client}>
        <TrashPanel />
      </QueryClientProvider>,
    ),
  };
}

describe('TrashPanel', () => {
  it('renders the empty state and distinguishes Trash from undeploy', async () => {
    arrange([]);
    expect(await screen.findByRole('heading', { name: 'Trash is empty' })).toBeInTheDocument();
    expect(screen.getByText(/Undeploying a Skill does not move it to Trash/i)).toBeInTheDocument();
  });

  it('requires the exact display name before planning permanent deletion', async () => {
    const user = userEvent.setup();
    const planDelete = vi.spyOn(api, 'trashPermanentDeletePlan').mockResolvedValue(reviewedPlan);
    arrange();

    const button = await screen.findByRole('button', { name: 'Plan permanent delete' });
    const input = screen.getByLabelText(/Enter Example Skill exactly/);
    expect(button).toBeDisabled();
    await user.type(input, 'example skill');
    expect(button).toBeDisabled();
    expect(planDelete).not.toHaveBeenCalled();
    await user.clear(input);
    await user.type(input, 'Example Skill');
    expect(button).toBeEnabled();
    await user.click(button);
    expect(planDelete).toHaveBeenCalledWith('trash-1', 'Example Skill');
  });

  it('shows a plan for review before explicit restore execution', async () => {
    const user = userEvent.setup();
    vi.spyOn(api, 'trashRestorePlan').mockResolvedValue(reviewedPlan);
    const execute = vi.spyOn(api, 'trashExecute').mockResolvedValue({
      operationId: 'operation-1',
      outcome: 'succeeded',
      replayed: false,
      succeeded: true,
      tone: 'success',
    });
    arrange();

    await user.click(await screen.findByRole('button', { name: 'Plan restore' }));
    expect(await screen.findByRole('heading', { name: 'Restore plan' })).toBeInTheDocument();
    expect(screen.getByText(/destinationRelativePath/)).toBeInTheDocument();
    expect(execute).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Execute reviewed restore' }));
    await waitFor(() =>
      expect(execute).toHaveBeenCalledWith('restore', {
        operationId: 'operation-1',
        planDigest: 'digest-1',
      }),
    );
    expect(await screen.findByText(/durable outcome recorded/)).toBeInTheDocument();
  });

  it('does not optimistically remove an entry while execution is pending', async () => {
    const user = userEvent.setup();
    vi.spyOn(api, 'trashRestorePlan').mockResolvedValue(reviewedPlan);
    let resolveExecution!: (value: import('../bindings').TrashExecutionView) => void;
    vi.spyOn(api, 'trashExecute').mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveExecution = resolve;
        }),
    );
    arrange();

    await user.click(await screen.findByRole('button', { name: 'Plan restore' }));
    await user.click(await screen.findByRole('button', { name: 'Execute reviewed restore' }));
    expect(screen.getAllByText('Example Skill').length).toBeGreaterThan(0);
    expect(screen.getByRole('option', { name: /Example Skill/ })).toBeInTheDocument();
    resolveExecution({
      operationId: 'operation-1',
      outcome: 'succeeded',
      replayed: false,
      succeeded: true,
      tone: 'success',
    });
    await screen.findByText(/durable outcome recorded/);
  });

  it('retains the reviewed plan and reports a non-success replay distinctly', async () => {
    const user = userEvent.setup();
    vi.spyOn(api, 'trashRestorePlan').mockResolvedValue(reviewedPlan);
    vi.spyOn(api, 'trashExecute').mockResolvedValue({
      operationId: 'operation-1',
      outcome: 'rolled_back',
      replayed: true,
      succeeded: false,
      tone: 'danger',
    });
    arrange();

    await user.click(await screen.findByRole('button', { name: 'Plan restore' }));
    await user.click(await screen.findByRole('button', { name: 'Execute reviewed restore' }));

    expect(await screen.findByText('rolled_back')).toBeInTheDocument();
    expect(screen.getByText(/plan retained for recovery or inspection/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Execute reviewed restore' })).toBeInTheDocument();
  });
});
