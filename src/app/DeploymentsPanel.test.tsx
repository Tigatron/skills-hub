import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { DeploymentHealthView, TargetView } from '../bindings';
import { api } from '../lib/api';
import { DeploymentsPanel } from './DeploymentsPanel';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      deploymentsList: vi.fn(),
      targetsList: vi.fn(),
      deploymentVerify: vi.fn(),
      undeployPlan: vi.fn(),
      operationExecute: vi.fn(),
      operationCancel: vi.fn(),
    },
  };
});

const deployment: DeploymentHealthView = {
  deploymentId: 'deployment-1',
  skillId: 'skill-1',
  targetId: 'target-1',
  deploymentName: 'Long-lived formatter',
  targetPath: '/Users/example/a/very/long/project/path/.agent/skills/long-lived-formatter',
  mode: 'symlink',
  active: true,
  health: 'target_modified',
  explanation: 'Target content differs from the reviewed deployment.',
  expectedDigest: 'expected',
  vaultDigest: 'vault',
  targetDigest: 'target',
  expectedLinkTarget: null,
  actualLinkTarget: null,
  driftDirection: 'target_ahead',
  allowedActions: ['verify', 'undeploy_preserve'],
  disabledReason: null,
  verifiedAt: '2026-08-05T00:00:00Z',
};

const target: TargetView = {
  targetId: 'target-1',
  adapterId: 'claude-code',
  scope: 'project',
  projectId: 'project-1',
  projectKind: 'git',
  rootPath: '/Users/example/a/very/long/project/path',
  isOverride: false,
  isCustom: false,
  defaultMode: 'symlink',
};

function renderPanel(item: DeploymentHealthView = deployment) {
  vi.mocked(api.deploymentsList).mockResolvedValue({ items: [item], count: 1 });
  vi.mocked(api.targetsList).mockResolvedValue([target]);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<DeploymentsPanel />, {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  });
}

describe('DeploymentsPanel', () => {
  beforeEach(() => vi.clearAllMocks());

  it('exposes identical backend item data in matrix and list fallback', async () => {
    const user = userEvent.setup();
    renderPanel();
    await screen.findByRole('button', { name: `Select ${deployment.deploymentName}` });
    const matrix = await screen.findByRole('table', { name: 'Deployment matrix' });
    expect(within(matrix).getByText(deployment.deploymentName)).toBeInTheDocument();
    expect(within(matrix).getByText(deployment.health)).toBeInTheDocument();
    expect(within(matrix).getByText(deployment.driftDirection)).toBeInTheDocument();
    expect(within(matrix).getByText(deployment.targetPath)).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'List' }));
    const list = screen.getByLabelText('Deployment list');
    expect(within(list).getByText(deployment.deploymentName)).toBeInTheDocument();
    expect(within(list).getByText(deployment.health)).toBeInTheDocument();
    expect(within(list).getByText(deployment.driftDirection)).toBeInTheDocument();
    expect(within(list).getByText(deployment.targetPath)).toBeInTheDocument();
  });

  it('supports keyboard focus and activation in both display modes', async () => {
    const user = userEvent.setup();
    renderPanel();
    const matrixSelect = await screen.findByRole('button', {
      name: `Select ${deployment.deploymentName}`,
    });
    await user.tab();
    while (document.activeElement !== matrixSelect) await user.tab();
    expect(matrixSelect).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(screen.getByRole('heading', { name: deployment.deploymentName })).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: 'List' }));
    const listSelect = screen.getByRole('button', { name: `Select ${deployment.deploymentName}` });
    await user.tab();
    while (document.activeElement !== listSelect) await user.tab();
    await user.keyboard(' ');
    expect(listSelect).toHaveFocus();
  });

  it('uses allowedActions and explains the backend disabled reason', async () => {
    const user = userEvent.setup();
    renderPanel({
      ...deployment,
      allowedActions: ['verify'],
      disabledReason: 'Managed target ownership could not be proven.',
    });
    await user.click(
      await screen.findByRole('button', { name: `Select ${deployment.deploymentName}` }),
    );

    expect(screen.getByRole('button', { name: 'Verify selected' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Plan redeploy' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Plan clean undeploy' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Plan preserve undeploy' })).toBeDisabled();
    expect(screen.getByRole('note')).toHaveTextContent(
      'Backend note: Managed target ownership could not be proven.',
    );
  });
});
