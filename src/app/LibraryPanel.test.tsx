import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { LibraryItem, LibraryPage, SkillDetail } from '../bindings';
import { api } from '../lib/api';
import { LibraryPanel, SkillSummary } from './LibraryPanel';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      ...actual.api,
      libraryList: vi.fn(),
      targetsList: vi.fn(),
      trashEntriesList: vi.fn(),
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
  snapshot: {
    available: false,
    count: 0,
    latestCreatedAt: null,
    protectedCount: 0,
    unavailableReason: 'No snapshots',
  },
  activity: [],
  capabilities: [],
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
    renderSummary({ ownership: 'external', capabilities: [] });
    expect(screen.queryByText(/trash|delete/i)).not.toBeInTheDocument();
  });

  it('gates active and trashed actions from backend capabilities', () => {
    const { unmount } = renderSummary({
      capabilities: [{ action: 'move_to_trash', allowed: true, disabledReason: null }],
    });
    expect(screen.getByRole('button', { name: 'Plan Move to Trash' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Plan restore' })).not.toBeInTheDocument();
    unmount();

    renderSummary({
      lifecycle: 'trashed',
      capabilities: [
        { action: 'restore', allowed: true, disabledReason: null },
        { action: 'permanently_delete', allowed: true, disabledReason: null },
      ],
    });
    expect(screen.getByRole('button', { name: 'Plan restore' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Plan Move to Trash' })).not.toBeInTheDocument();
  });

  it('renders authoritative snapshot, linked activity, and disabled reasons', () => {
    renderSummary({
      snapshot: {
        available: true,
        count: 3,
        latestCreatedAt: '2026-08-04T10:00:00Z',
        protectedCount: 1,
        unavailableReason: null,
      },
      activity: [
        {
          activityId: 'activity-1',
          kind: 'deploy',
          summary: 'Deployment rolled back',
          operationId: 'operation-1',
          outcome: 'rolled_back',
          startedAt: '2026-08-04T09:00:00Z',
          undoCheckAvailable: false,
          undoCheckReason: 'Target has drifted',
        },
      ],
      capabilities: [{ action: 'deploy', allowed: false, disabledReason: 'Skill is trashed' }],
    });

    expect(screen.getByText(/3 retained · 1 protected · latest 2026-08-04/)).toBeInTheDocument();
    expect(screen.getByText(/Deployment rolled back · rolled_back/)).toHaveTextContent(
      'Undo: Target has drifted',
    );
    expect(screen.getByText('deploy unavailable: Skill is trashed')).toBeInTheDocument();
  });

  it('requires the exact Skill name before permanent delete planning', async () => {
    const user = userEvent.setup();
    const onPlan = vi.fn();
    const { rerender } = render(
      <SkillSummary
        detail={{
          ...detail,
          lifecycle: 'trashed',
          capabilities: [{ action: 'permanently_delete', allowed: true, disabledReason: null }],
        }}
        busy={false}
        confirmation="wrong"
        onConfirmationChange={() => undefined}
        onPlan={onPlan}
      />,
    );
    expect(screen.getByRole('button', { name: 'Plan permanent delete' })).toBeDisabled();
    rerender(
      <SkillSummary
        detail={{
          ...detail,
          lifecycle: 'trashed',
          capabilities: [{ action: 'permanently_delete', allowed: true, disabledReason: null }],
        }}
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

const item = (ownership: LibraryItem['ownership'], index: number): LibraryItem => ({
  id: `item-${index}`,
  skillId: ownership === 'external' ? null : `skill-${index}`,
  displayName: `${ownership} skill ${index}`,
  deploymentName: `${ownership}-skill-${index}`,
  ownership,
  sourceSummary: 'Universal global',
  locations: [
    {
      observationId: `observation-${index}`,
      adapterId: 'universal',
      sourceRootId: 'global',
      path: `/skills/${index}`,
      status: 'observed',
      error: null,
    },
  ],
  digest: `digest-${index}`,
  validation: 'verified',
  duplicateSummary: {
    exactDuplicateLocations: 0,
    nameConflicts: 0,
    probableDuplicatesOrRenames: 0,
    unverified: false,
  },
  deploymentCount: ownership === 'managed' ? 1 : 0,
  workingLocation: ownership === 'external' ? null : `/vault/skills/${index}`,
  changedAt: '2026-08-05T00:00:00Z',
  nextActions: ownership === 'external' ? ['keep_external'] : [],
});

function page(items: LibraryItem[]): LibraryPage {
  return { items, total: items.length, offset: 0, limit: 100 };
}

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<LibraryPanel />, {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  });
}

describe('LibraryPanel authoritative query rendering', () => {
  beforeEach(() => {
    vi.mocked(api.targetsList).mockResolvedValue([]);
    vi.mocked(api.trashEntriesList).mockResolvedValue([]);
  });

  it('renders external, vaulted, and managed ownership returned by Rust', async () => {
    vi.mocked(api.libraryList).mockResolvedValue(
      page([item('external', 1), item('vaulted', 2), item('managed', 3)]),
    );
    renderPanel();

    expect(await screen.findByText('external skill 1')).toBeInTheDocument();
    expect(screen.getByText(/external · Universal global/)).toBeInTheDocument();
    expect(screen.getByText(/vaulted · Universal global/)).toBeInTheDocument();
    expect(screen.getByText(/managed · Universal global/)).toBeInTheDocument();
  });

  it('propagates trimmed search and filter changes to the backend query', async () => {
    vi.mocked(api.libraryList).mockResolvedValue(page([]));
    const user = userEvent.setup();
    renderPanel();
    await screen.findByRole('heading', { name: 'No Skills observed yet' });

    await user.type(screen.getByRole('textbox', { name: 'Filter library' }), '  finder  ');
    await user.selectOptions(screen.getByRole('combobox', { name: 'Library filter' }), 'conflicts');

    await waitFor(() =>
      expect(api.libraryList).toHaveBeenLastCalledWith({
        offset: 0,
        limit: 100,
        search: 'finder',
        filter: 'conflicts',
      }),
    );
  });

  it('virtualizes a large page into a bounded subset on a virtual canvas', async () => {
    vi.mocked(api.libraryList).mockResolvedValue(
      page(Array.from({ length: 100 }, (_, index) => item('external', index))),
    );
    renderPanel();

    const list = await screen.findByTestId('library-virtual-list');
    await waitFor(() => expect(list.querySelectorAll('[data-index]').length).toBeGreaterThan(0));
    expect(list.querySelectorAll('[data-index]').length).toBeLessThan(100);
    expect(Number.parseFloat((list.firstElementChild as HTMLElement).style.height)).toBeGreaterThan(
      620,
    );
  });
});
