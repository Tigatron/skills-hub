import { QueryClient } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';

import { invalidateFromDomainEvent } from './query';

describe('domain invalidation routing', () => {
  it.each([
    ['scan', ['library', 'scan', 'workspace-roots', 'manual-projects']],
    ['library', ['library', 'scan', 'skill']],
    ['workspace', ['workspace-roots', 'manual-projects']],
    ['project', ['workspace-roots', 'manual-projects']],
    ['skill', ['skill', 'trash', 'trash-retention']],
    ['deployment', ['deployments', 'targets']],
    ['deployments', ['deployments', 'targets']],
    ['target', ['targets', 'configured-adapters']],
    ['adapter', ['targets', 'configured-adapters']],
    ['settings', ['targets', 'configured-adapters']],
    ['activity', ['activity', 'operation']],
    ['operation', ['activity', 'operation']],
    ['vault', ['bootstrap', 'vault-status', 'vault-verify', 'vault-gc-settings']],
    ['bootstrap', ['bootstrap', 'vault-status', 'vault-verify', 'vault-gc-settings']],
    ['trash', ['trash', 'trash-retention']],
  ])('keeps a refetch path for the %s scope', (scope, expected) => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries').mockResolvedValue();

    invalidateFromDomainEvent(client, { revision: 1, scopes: [scope], ids: [] });

    const firstKeys = invalidate.mock.calls.map(([filters]) => String(filters?.queryKey?.[0]));
    expect(firstKeys).toEqual(expect.arrayContaining(expected));
  });

  it('uses the safe Library and scan fallback when scopes are empty', () => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries').mockResolvedValue();
    invalidateFromDomainEvent(client, { revision: 1, scopes: [], ids: [] });
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey?.[0])).toEqual([
      'library',
      'scan',
    ]);
  });
});
