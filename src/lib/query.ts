import type { QueryClient } from '@tanstack/react-query';

import type { DomainInvalidated } from '../bindings';

export const queryKeys = {
  bootstrap: ['bootstrap'] as const,
  vaultStatus: ['vault-status'] as const,
  library: (filters: { search: string; filter: string; offset: number }) =>
    ['library', filters] as const,
  skill: (skillId: string) => ['skill', skillId] as const,
  targets: ['targets'] as const,
  deployments: (filters: { skillId: string | null; includeInactive: boolean }) =>
    ['deployments', filters] as const,
  activity: (filters: { kind: string | null; outcome: string | null }) =>
    ['activity', filters] as const,
  activityDetail: (id: string) => ['activity-detail', id] as const,
  operation: (operationId: string) => ['operation', operationId] as const,
  scan: (jobId: string) => ['scan', jobId] as const,
};

export function invalidateFromDomainEvent(client: QueryClient, event: DomainInvalidated) {
  const scopes = new Set(event.scopes.map((scope) => scope.toLowerCase()));
  if (scopes.has('scan') || scopes.has('library') || scopes.size === 0) {
    void client.invalidateQueries({ queryKey: ['library'] });
    void client.invalidateQueries({ queryKey: ['scan'] });
  }
  if (scopes.has('skill') || scopes.has('library')) {
    void client.invalidateQueries({ queryKey: ['skill'] });
  }
  if (scopes.has('deployment') || scopes.has('deployments')) {
    void client.invalidateQueries({ queryKey: ['deployments'] });
    void client.invalidateQueries({ queryKey: ['targets'] });
  }
  if (scopes.has('activity') || scopes.has('operation')) {
    void client.invalidateQueries({ queryKey: ['activity'] });
    void client.invalidateQueries({ queryKey: ['operation'] });
  }
  if (scopes.has('vault') || scopes.has('bootstrap')) {
    void client.invalidateQueries({ queryKey: queryKeys.bootstrap });
    void client.invalidateQueries({ queryKey: queryKeys.vaultStatus });
  }
}

export function invalidateAfterOperation(client: QueryClient) {
  void client.invalidateQueries({ queryKey: ['library'] });
  void client.invalidateQueries({ queryKey: ['skill'] });
  void client.invalidateQueries({ queryKey: ['deployments'] });
  void client.invalidateQueries({ queryKey: ['targets'] });
  void client.invalidateQueries({ queryKey: ['activity'] });
  void client.invalidateQueries({ queryKey: ['operation'] });
  void client.invalidateQueries({ queryKey: queryKeys.bootstrap });
}
