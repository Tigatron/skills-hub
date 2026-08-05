import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';

import type { VaultStatusView } from '../bindings';
import { api } from '../lib/api';
import { queryKeys } from '../lib/query';
import { ErrorBanner, PathText, PrimaryButton, SecondaryButton } from './components';
import styles from './thin.module.css';

export function FirstRun({ status }: { status: VaultStatusView }) {
  const queryClient = useQueryClient();
  const [customPath, setCustomPath] = useState('');
  const [useCustom, setUseCustom] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const selectedPath = useCustom && customPath.trim() ? customPath.trim() : status.defaultPath;

  const initialize = useMutation({
    mutationFn: () =>
      api.vaultInitialize({
        selectedDirectory: useCustom && customPath.trim() ? customPath.trim() : null,
      }),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: queryKeys.bootstrap }),
        queryClient.invalidateQueries({ queryKey: queryKeys.vaultStatus }),
      ]);
    },
  });

  return (
    <section className={styles.firstRun} aria-labelledby="first-run-title">
      <h1 id="first-run-title">Create a Vault</h1>
      <p>
        Skills Hub stores an index and recovery metadata in a Vault you own. Scanning stays
        read-only. Deployment and takeover only run after you review a plan.
      </p>

      <ul className={styles.checklist}>
        <li>
          <span aria-hidden="true">1</span>
          <span>Initialize a Vault on this Mac</span>
        </li>
        <li>
          <span aria-hidden="true">2</span>
          <span>Scan the Universal global Skills root</span>
        </li>
        <li>
          <span aria-hidden="true">3</span>
          <span>Add a Skill to the Vault, deploy, verify, and undeploy</span>
        </li>
      </ul>

      <div className={styles.stack} style={{ marginTop: 20 }}>
        <div className={styles.metaList}>
          <div className={styles.metaRow}>
            <dt>Default path</dt>
            <dd>
              <PathText path={status.defaultPath} />
            </dd>
          </div>
        </div>

        <label className={styles.inlineFields}>
          <input
            type="checkbox"
            checked={useCustom}
            onChange={(event) => setUseCustom(event.target.checked)}
          />
          <span>Use a custom Vault directory</span>
        </label>

        {useCustom ? (
          <input
            className={styles.textInput}
            value={customPath}
            onChange={(event) => setCustomPath(event.target.value)}
            placeholder="/absolute/path/to/Vault"
            aria-label="Custom Vault directory"
          />
        ) : null}

        {initialize.isError ? <ErrorBanner error={initialize.error} /> : null}

        {confirming ? (
          <div className={styles.planBox} role="alert">
            <h2>Confirm Vault creation</h2>
            <p>Skills Hub will create its managed Vault structure at this exact path:</p>
            <PathText path={selectedPath} />
            <p className={styles.muted}>
              This creates ordinary working directories plus the .manager index, manifests,
              operation journals, snapshots, and recovery metadata. It does not scan, import,
              deploy, overwrite, or delete any existing Skill.
            </p>
          </div>
        ) : null}

        <div className={styles.firstRunActions}>
          <PrimaryButton
            onPress={() => (confirming ? initialize.mutate() : setConfirming(true))}
            isDisabled={initialize.isPending || (useCustom && !customPath.trim())}
          >
            {initialize.isPending
              ? 'Initializing…'
              : confirming
                ? `Create Vault at ${selectedPath}`
                : 'Review Vault creation'}
          </PrimaryButton>
          {confirming ? (
            <SecondaryButton onPress={() => setConfirming(false)} isDisabled={initialize.isPending}>
              Go back
            </SecondaryButton>
          ) : null}
        </div>
      </div>
    </section>
  );
}
