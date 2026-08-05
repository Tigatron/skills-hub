import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import { Button } from 'react-aria-components';

import { api, listenDomainInvalidated, type NavId } from '../lib/api';
import { invalidateFromDomainEvent, queryKeys } from '../lib/query';
import { ActivityPanel } from './ActivityPanel';
import { ErrorBanner, LoadingBlock, PrimaryButton } from './components';
import { DeploymentsPanel } from './DeploymentsPanel';
import { FirstRun } from './FirstRun';
import { LibraryPanel } from './LibraryPanel';
import styles from './thin.module.css';

export function App() {
  const queryClient = useQueryClient();
  const [nav, setNav] = useState<NavId>('library');
  const [setupDismissed, setSetupDismissed] = useState(false);

  const bootstrap = useQuery({
    queryKey: queryKeys.bootstrap,
    queryFn: () => api.bootstrapGetState(),
  });

  const vaultStatus = useQuery({
    queryKey: queryKeys.vaultStatus,
    queryFn: () => api.vaultStatus(),
    enabled: bootstrap.isSuccess,
  });

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void (async () => {
      try {
        const stop = await listenDomainInvalidated((event) => {
          invalidateFromDomainEvent(queryClient, event);
        });
        if (cancelled) {
          stop();
          return;
        }
        unlisten = stop;
      } catch {
        // Renderer may run outside Tauri during unit tests.
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [queryClient]);

  if (bootstrap.isPending || (bootstrap.isSuccess && vaultStatus.isPending)) {
    return (
      <main className={styles.shell}>
        <Titlebar stage="connecting" />
        <div className={styles.mainContent}>
          <LoadingBlock label="Connecting to the Rust runtime" />
        </div>
      </main>
    );
  }

  if (bootstrap.isError) {
    return (
      <main className={styles.shell}>
        <Titlebar stage="unavailable" />
        <div className={styles.mainContent}>
          <ErrorBanner error={bootstrap.error} onRetry={() => void bootstrap.refetch()} />
        </div>
      </main>
    );
  }

  const vaultReady = Boolean(bootstrap.data?.vaultInitialized || vaultStatus.data?.initialized);

  if (!vaultReady && vaultStatus.data) {
    return (
      <main className={styles.shell}>
        <Titlebar stage={bootstrap.data?.implementationStage ?? 'M0-009'} />
        <div className={styles.mainContent}>
          <FirstRun status={vaultStatus.data} />
        </div>
      </main>
    );
  }

  if (!vaultReady && vaultStatus.isError) {
    return (
      <main className={styles.shell}>
        <Titlebar stage="vault" />
        <div className={styles.mainContent}>
          <ErrorBanner error={vaultStatus.error} onRetry={() => void vaultStatus.refetch()} />
        </div>
      </main>
    );
  }

  return (
    <main className={styles.shell}>
      <Titlebar stage={bootstrap.data?.implementationStage ?? 'M0-009'} />
      <div className={styles.body}>
        <nav className={styles.nav} aria-label="Primary">
          <div className={styles.navGroup}>
            <div className={styles.navLabel}>Work</div>
            <NavButton id="library" current={nav} onSelect={setNav} label="Library" />
            <NavButton id="deployments" current={nav} onSelect={setNav} label="Deployments" />
            <NavButton id="activity" current={nav} onSelect={setNav} label="Activity" />
          </div>
          <div className={styles.navFoot}>
            <div>Vault</div>
            <div>{bootstrap.data?.vaultPath ?? vaultStatus.data?.rootPath ?? 'Initialized'}</div>
            <div style={{ marginTop: 8 }}>
              Runtime {bootstrap.data?.runtimeStatus} · workers{' '}
              {bootstrap.data?.blockingWorkerLimit}
            </div>
          </div>
        </nav>

        <div className={styles.main}>
          {!setupDismissed ? (
            <div className={styles.noticeBar}>
              <span>
                Setup checklist: Vault ready → scan Universal global → Add to Vault → deploy →
                verify → undeploy. Scans report “No files were changed.”
              </span>
              <PrimaryButton onPress={() => setSetupDismissed(true)}>Dismiss</PrimaryButton>
            </div>
          ) : null}
          <div className={styles.mainContent}>
            {nav === 'library' ? <LibraryPanel /> : null}
            {nav === 'deployments' ? <DeploymentsPanel /> : null}
            {nav === 'activity' ? <ActivityPanel /> : null}
          </div>
        </div>
      </div>
    </main>
  );
}

function Titlebar({ stage }: { stage: string }) {
  return (
    <header className={styles.titlebar} data-tauri-drag-region>
      <div className={styles.brand} aria-label="Skills Hub">
        <span className={styles.brandMark} aria-hidden="true">
          SH
        </span>
        <span>Skills Hub</span>
      </div>
      <span className={styles.buildLabel}>{stage}</span>
    </header>
  );
}

function NavButton({
  id,
  current,
  onSelect,
  label,
}: {
  id: NavId;
  current: NavId;
  onSelect: (id: NavId) => void;
  label: string;
}) {
  const selected = current === id;
  return (
    <Button
      className={styles.navButton!}
      onPress={() => onSelect(id)}
      data-selected={selected}
      {...(selected ? { 'aria-current': 'page' as const } : {})}
    >
      <span>{label}</span>
    </Button>
  );
}
