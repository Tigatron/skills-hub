import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { LoadingBlock, PathText, StatusPill } from './components';

describe('static accessibility primitives', () => {
  it('announces loading with a polite live region and visible-hidden label', () => {
    render(<LoadingBlock label="Loading library" />);
    const status = screen.getByRole('status');
    expect(status).toHaveAttribute('aria-busy', 'true');
    expect(status).toHaveAttribute('aria-live', 'polite');
    expect(screen.getByText('Loading library')).toBeInTheDocument();
  });

  it('pairs status text with a tone icon and non-color label', () => {
    render(<StatusPill tone="danger">broken_link</StatusPill>);
    expect(screen.getByText('Error:')).toBeInTheDocument();
    expect(screen.getByText('broken_link')).toBeInTheDocument();
    expect(screen.getByText('!')).toHaveAttribute('aria-hidden', 'true');
  });
});

describe('PathText', () => {
  it('copies every path and invokes optional reveal', async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    const reveal = vi.fn();
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    render(<PathText path="/a/very/long/path" onReveal={reveal} />);

    await user.click(screen.getByRole('button', { name: 'Copy /a/very/long/path' }));
    expect(writeText).toHaveBeenCalledWith('/a/very/long/path');
    expect(screen.getByText('Path copied')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Reveal /a/very/long/path' }));
    expect(reveal).toHaveBeenCalledOnce();
  });

  it('announces an unavailable reveal when the backend action rejects', async () => {
    const user = userEvent.setup();
    render(<PathText path="/missing/path" onReveal={() => Promise.reject(new Error('missing'))} />);

    await user.click(screen.getByRole('button', { name: 'Reveal /missing/path' }));
    expect(await screen.findByText('Could not reveal path')).toBeInTheDocument();
  });
});
