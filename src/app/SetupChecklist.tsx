import { useQuery } from '@tanstack/react-query';
import { useState } from 'react';

import { api } from '../lib/api';
import { queryKeys } from '../lib/query';
import { SecondaryButton } from './components';
import styles from './SetupChecklist.module.css';

const STORAGE_KEY = 'skills-hub.m0-setup-checklist-dismissed';

export function SetupChecklist() {
  const [dismissed, setDismissed] = useState(readDismissed);
  const roots = useQuery({ queryKey: queryKeys.workspaceRoots, queryFn: api.workspaceRootsList });
  const targets = useQuery({ queryKey: queryKeys.targets, queryFn: api.targetsList });
  const deployments = useQuery({
    queryKey: queryKeys.deployments({ skillId: null, includeInactive: true }),
    queryFn: () =>
      api.deploymentsList({ skillId: null, targetId: null, includeInactive: true, limit: 1 }),
  });

  const setVisibility = (hidden: boolean) => {
    try {
      window.localStorage?.setItem(STORAGE_KEY, String(hidden));
    } catch {
      // Persistence may be unavailable in hardened webviews; the session state still updates.
    }
    setDismissed(hidden);
  };

  if (dismissed) {
    return (
      <div className={styles.collapsed}>
        <span>Setup checklist hidden</span>
        <SecondaryButton onPress={() => setVisibility(false)}>Show checklist</SecondaryButton>
      </div>
    );
  }

  const checks = [
    { label: 'Vault initialized', complete: true },
    { label: 'Workspace source configured', complete: Boolean(roots.data?.length) },
    { label: 'Deployment target available', complete: Boolean(targets.data?.length) },
    { label: 'First deployment recorded', complete: Boolean(deployments.data?.count) },
  ];
  const complete = checks.filter((check) => check.complete).length;

  return (
    <section className={styles.checklist} aria-label="Setup checklist">
      <div>
        <strong>
          Setup {complete}/{checks.length}
        </strong>
        <ul>
          {checks.map((check) => (
            <li key={check.label} data-complete={check.complete}>
              <span aria-hidden="true">{check.complete ? '✓' : '○'}</span>
              {check.label}
            </li>
          ))}
        </ul>
      </div>
      <SecondaryButton onPress={() => setVisibility(true)}>Hide</SecondaryButton>
    </section>
  );
}

function readDismissed() {
  try {
    return window.localStorage?.getItem(STORAGE_KEY) === 'true';
  } catch {
    return false;
  }
}
