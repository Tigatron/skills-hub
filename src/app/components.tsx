import { useState, type ReactNode } from 'react';
import { Button } from 'react-aria-components';

import { CommandError } from '../lib/api';
import styles from './thin.module.css';

export function PanelHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
}) {
  return (
    <div className={styles.panelHeader}>
      <div>
        <h2>{title}</h2>
        {description ? <p>{description}</p> : null}
      </div>
      {actions ? <div className={styles.panelActions}>{actions}</div> : null}
    </div>
  );
}

type SimpleButtonProps = {
  children: ReactNode;
  onPress?: () => void;
  isDisabled?: boolean;
};

export function PrimaryButton({ children, onPress, isDisabled = false }: SimpleButtonProps) {
  return (
    <Button
      className={styles.primaryButton!}
      onPress={onPress ?? (() => undefined)}
      isDisabled={isDisabled}
    >
      {children}
    </Button>
  );
}

export function SecondaryButton({ children, onPress, isDisabled = false }: SimpleButtonProps) {
  return (
    <Button
      className={styles.secondaryButton!}
      onPress={onPress ?? (() => undefined)}
      isDisabled={isDisabled}
    >
      {children}
    </Button>
  );
}

export function DangerButton({ children, onPress, isDisabled = false }: SimpleButtonProps) {
  return (
    <Button
      className={styles.dangerButton!}
      onPress={onPress ?? (() => undefined)}
      isDisabled={isDisabled}
    >
      {children}
    </Button>
  );
}

export function StatusPill({
  tone,
  children,
}: {
  tone: 'neutral' | 'success' | 'pending' | 'danger';
  children: ReactNode;
}) {
  return (
    <span className={styles.statusPill} data-tone={tone}>
      {children}
    </span>
  );
}

export function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <div className={styles.emptyState}>
      <h3>{title}</h3>
      <p>{body}</p>
    </div>
  );
}

export function ErrorBanner({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const details = describeError(error);
  return (
    <div className={styles.errorBanner} role="alert">
      <div>
        <strong>{details.title}</strong>
        <p>{details.message}</p>
        {details.recovery ? <p className={styles.muted}>Next: {details.recovery}</p> : null}
      </div>
      {onRetry ? <SecondaryButton onPress={onRetry}>Retry</SecondaryButton> : null}
    </div>
  );
}

function describeError(error: unknown): {
  title: string;
  message: string;
  recovery: string | null;
} {
  if (error instanceof CommandError) {
    return {
      title: error.details.title,
      message: error.details.message,
      recovery: error.details.recoveryAction,
    };
  }
  if (error && typeof error === 'object' && 'message' in error) {
    return {
      title: 'Request failed',
      message: String((error as { message: unknown }).message),
      recovery: null,
    };
  }
  return {
    title: 'Request failed',
    message: 'Skills Hub could not complete that request. No optimistic state was applied.',
    recovery: null,
  };
}

export function LoadingBlock({ label }: { label: string }) {
  return (
    <div className={styles.loadingBlock} aria-label={label}>
      <span />
      <span />
      <span />
    </div>
  );
}

export function MetaRow({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className={styles.metaRow}>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

export function PathText({
  path,
  onReveal,
}: {
  path: string;
  onReveal?: () => void | Promise<unknown>;
}) {
  const [status, setStatus] = useState<string | null>(null);
  const copy = async () => {
    try {
      if (!navigator.clipboard) throw new Error('Clipboard access is unavailable');
      await navigator.clipboard.writeText(path);
      setStatus('Path copied');
    } catch {
      setStatus('Could not copy path');
    }
  };
  const reveal = async () => {
    try {
      await onReveal?.();
      setStatus('Revealed in Finder');
    } catch {
      setStatus('Could not reveal path');
    }
  };
  return (
    <span className={styles.pathGroup}>
      <code className={styles.pathText} title={path}>
        {path}
      </code>
      <span className={styles.pathActions}>
        <Button
          className={styles.pathAction!}
          onPress={() => void copy()}
          aria-label={`Copy ${path}`}
        >
          Copy
        </Button>
        {onReveal ? (
          <Button
            className={styles.pathAction!}
            onPress={() => void reveal()}
            aria-label={`Reveal ${path}`}
          >
            Reveal
          </Button>
        ) : null}
      </span>
      {status ? (
        <span className={styles.visuallyHidden} aria-live="polite">
          {status}
        </span>
      ) : null}
    </span>
  );
}
