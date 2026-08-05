import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type { SkillDetail } from '../bindings';
import { SkillSummary } from './LibraryPanel';

const detail: SkillDetail = {
  skillId: 'skill-1',
  displayName: 'Example Skill',
  deploymentName: 'example-skill',
  workingPath: 'skills/skill-1/example-skill',
  workingDigest: 'digest',
  baselineDigest: 'digest',
  ownership: 'vaulted',
  lifecycle: 'active',
  sourcePaths: [],
  deploymentPaths: [],
  observationPaths: [],
  conflicts: [],
  allowedActions: [],
};

function renderSummary(overrides: Partial<SkillDetail>, onPlan = vi.fn()) {
  const props = { ...detail, ...overrides };
  const result = render(
    <SkillSummary
      detail={props}
      busy={false}
      confirmation=""
      onConfirmationChange={() => undefined}
      onPlan={onPlan}
    />,
  );
  return { ...result, onPlan };
}

describe('managed Skill lifecycle actions', () => {
  it('does not invent delete actions for an external/non-managed read model', () => {
    renderSummary({ ownership: 'external', allowedActions: ['takeover', 'ignore'] });
    expect(screen.queryByText(/trash|delete/i)).not.toBeInTheDocument();
  });

  it('gates active and trashed actions from allowedActions', () => {
    const { unmount } = renderSummary({ allowedActions: ['move_to_trash'] });
    expect(screen.getByRole('button', { name: 'Plan Move to Trash' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Plan restore' })).not.toBeInTheDocument();
    unmount();

    renderSummary({
      lifecycle: 'trashed',
      allowedActions: ['restore', 'permanently_delete'],
    });
    expect(screen.getByRole('button', { name: 'Plan restore' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Plan Move to Trash' })).not.toBeInTheDocument();
  });

  it('requires the exact Skill name before permanent delete planning', async () => {
    const user = userEvent.setup();
    const onPlan = vi.fn();
    const { rerender } = render(
      <SkillSummary
        detail={{ ...detail, lifecycle: 'trashed', allowedActions: ['permanently_delete'] }}
        busy={false}
        confirmation="wrong"
        onConfirmationChange={() => undefined}
        onPlan={onPlan}
      />,
    );
    expect(screen.getByRole('button', { name: 'Plan permanent delete' })).toBeDisabled();
    rerender(
      <SkillSummary
        detail={{ ...detail, lifecycle: 'trashed', allowedActions: ['permanently_delete'] }}
        busy={false}
        confirmation="Example Skill"
        onConfirmationChange={() => undefined}
        onPlan={onPlan}
      />,
    );
    await user.click(screen.getByRole('button', { name: 'Plan permanent delete' }));
    expect(onPlan).toHaveBeenCalledWith('permanently_delete');
  });
});
