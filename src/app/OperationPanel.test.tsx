import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { api, type ReviewedPlan } from '../lib/api';
import { OperationPanel } from './OperationPanel';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return { ...actual, api: { ...actual.api, operationPlanExport: vi.fn() } };
});

const plan: ReviewedPlan = {
  kind: 'deployment',
  plan: {
    operationId: 'operation-1',
    planDigest: 'sha256-operation-plan-v3:fixture',
    expiresAt: '2026-08-05T12:00:00Z',
    action: 'deploy',
    skillId: 'skill-1',
    targetId: 'target-1',
    deploymentId: 'deployment-1',
    targetPath: '/tmp/target/example',
    requestedMode: 'symlink',
    resolvedMode: 'symlink',
    fallbackReason: null,
    reviewedHealth: 'missing_target',
    noOp: false,
    consequence: 'Create a managed symlink.',
    recoveryCount: 1,
    executionAllowed: true,
  },
};

const trashPlan: ReviewedPlan = {
  kind: 'trash',
  action: 'restore',
  plan: {
    operationId: 'operation-trash',
    planDigest: 'digest-trash',
    entry: {
      entryId: 'entry-1',
      skillId: 'skill-1',
      displayName: 'Example Skill',
      originalWorkingPath: 'skills/original/example',
      trashedAt: '2026-08-05T12:00:00Z',
      retentionDeadline: null,
      retentionPolicy: 'never',
      protectedReferences: [],
    },
    blockers: [],
    executionAllowed: true,
  },
};

describe('Operation Plan export', () => {
  it('renders the exact backend-exported JSON without regenerating the plan in React', async () => {
    vi.mocked(api.operationPlanExport).mockResolvedValue({
      operationId: 'operation-1',
      planDigest: 'sha256-operation-plan-v3:fixture',
      json: '{\n  "operationId": "operation-1",\n  "action": "deploy"\n}',
    });
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <OperationPanel
          plan={plan}
          operation={null}
          busy={false}
          onExecute={() => undefined}
          onCancel={() => undefined}
          onClear={() => undefined}
        />
      </QueryClientProvider>,
    );

    await userEvent.click(screen.getByRole('button', { name: 'Export plan JSON' }));

    expect(api.operationPlanExport).toHaveBeenCalledWith({ operationId: 'operation-1' });
    expect((await screen.findByText(/"action": "deploy"/)).textContent).toMatchInlineSnapshot(`
      "{
        "operationId": "operation-1",
        "action": "deploy"
      }"
    `);
  });

  it('requires a matching persisted export before Trash execution is enabled', async () => {
    vi.mocked(api.operationPlanExport)
      .mockResolvedValueOnce({
        operationId: 'another-operation',
        planDigest: 'another-digest',
        json: '{}',
      })
      .mockResolvedValueOnce({
        operationId: 'operation-trash',
        planDigest: 'digest-trash',
        json: '{"destinationRelativePath":"skills/new/example"}',
      });
    const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
    render(
      <QueryClientProvider client={client}>
        <OperationPanel
          plan={trashPlan}
          operation={null}
          busy={false}
          onExecute={() => undefined}
          onCancel={() => undefined}
          onClear={() => undefined}
        />
      </QueryClientProvider>,
    );
    const execute = screen.getByRole('button', { name: 'Execute reviewed plan' });
    expect(execute).toBeDisabled();

    await userEvent.click(screen.getByRole('button', { name: 'Export plan JSON' }));
    expect(execute).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Export plan JSON' }));

    expect(await screen.findByText(/destinationRelativePath/)).toBeInTheDocument();
    expect(execute).toBeEnabled();
  });
});
