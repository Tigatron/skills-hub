import { useQuery } from '@tanstack/react-query';
import { Button } from 'react-aria-components';

import { getBootstrapState } from '../lib/bootstrap';
import styles from './App.module.css';

export function App() {
  const bootstrap = useQuery({
    queryKey: ['bootstrap'],
    queryFn: getBootstrapState,
  });

  return (
    <main className={styles.shell}>
      <header className={styles.titlebar} data-tauri-drag-region>
        <div className={styles.brand} aria-label="Skills Hub">
          <span className={styles.brandMark} aria-hidden="true">
            SH
          </span>
          <span>Skills Hub</span>
        </div>
        <span className={styles.buildLabel}>Foundation build</span>
      </header>

      <div className={styles.workspace}>
        <aside className={styles.context} aria-label="Foundation scope">
          <h1>Foundation ready</h1>
          <p className={styles.intro}>
            Skills Hub can start its native shell and verify typed IPC. This build cannot scan,
            import, deploy, or change local Skills.
          </p>

          <div className={styles.boundary}>
            <strong>No filesystem access</strong>
            <span>Vault and scanner work begins after the domain contracts.</span>
          </div>
        </aside>

        <section className={styles.content} aria-labelledby="runtime-heading">
          <div className={styles.sectionHeading}>
            <h2 id="runtime-heading">Runtime readiness</h2>
            <ConnectionStatus status={bootstrap.status} />
          </div>

          {bootstrap.isPending ? <LoadingState /> : null}
          {bootstrap.isError ? <ErrorState retry={() => void bootstrap.refetch()} /> : null}
          {bootstrap.data ? <ReadyState state={bootstrap.data} /> : null}
        </section>
      </div>
    </main>
  );
}

function ConnectionStatus({ status }: { status: 'pending' | 'error' | 'success' }) {
  const copy =
    status === 'success' ? 'Connected' : status === 'error' ? 'Unavailable' : 'Connecting';

  return (
    <span className={styles.connection} data-status={status} role="status">
      <span aria-hidden="true" />
      {copy}
    </span>
  );
}

function LoadingState() {
  return (
    <div className={styles.loading} aria-label="Connecting to the Rust runtime">
      <span />
      <span />
      <span />
      <span />
    </div>
  );
}

function ErrorState({ retry }: { retry: () => void }) {
  return (
    <div className={styles.errorState} role="alert">
      <h3>Rust backend unavailable</h3>
      <p>The renderer could not read its bootstrap contract. No files were changed.</p>
      <Button className={styles.retryButton!} onPress={retry}>
        Retry connection
      </Button>
    </div>
  );
}

function ReadyState({ state }: { state: Awaited<ReturnType<typeof getBootstrapState>> }) {
  const rows = [
    ['Desktop runtime', state.runtimeStatus, `${state.platform.os} · ${state.platform.arch}`],
    ['Typed contract', 'Generated', `Schema ${state.contractVersion}`],
    ['Blocking workers', 'Bounded', `${state.blockingWorkerLimit} concurrent jobs maximum`],
    ['Vault storage', 'Not initialized', 'Scheduled for M0-003'],
  ] as const;

  return (
    <div className={styles.readyState}>
      <dl className={styles.readinessList}>
        {rows.map(([label, value, detail]) => (
          <div key={label}>
            <dt>{label}</dt>
            <dd>
              <strong>{value}</strong>
              <span>{detail}</span>
            </dd>
          </div>
        ))}
      </dl>

      <footer className={styles.nextStep}>
        <div>
          <span>
            <small>Next implementation unit</small>
            <strong>M0-002 · Identity, hashing, and path contracts</strong>
          </span>
        </div>
        <code>{state.appVersion}</code>
      </footer>
    </div>
  );
}
