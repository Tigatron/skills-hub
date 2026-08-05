import { QueryClient } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';

import { invalidateFromDomainEvent } from './query';

describe('domain invalidation routing', () => {
  it.each([
    ['scan', ['library', 'scan', 'workspace-roots', 'manual-projects']],
    ['deployment', ['deployments', 'targets']],
    ['adapter', ['targets', 'configured-adapters']],
    ['activity', ['activity', 'operation']],
    ['vault', ['bootstrap', 'vault-status', 'vault-verify', 'vault-gc-settings']],
    ['trash', ['trash', 'trash-retention']],
  ])('keeps a refetch path for the %s scope', (scope, expected) => {
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries').mockResolvedValue();

    invalidateFromDomainEvent(client, { revision: 1, scopes: [scope], ids: [] });

    const firstKeys = invalidate.mock.calls.map(([filters]) => String(filters?.queryKey?.[0]));
    expect(firstKeys).toEqual(expect.arrayContaining(expected));
  });
});
