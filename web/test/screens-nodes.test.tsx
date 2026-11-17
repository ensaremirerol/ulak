import { fireEvent, render, screen } from '@testing-library/preact';
import { beforeEach, describe, expect, it } from 'vitest';
import type { UiToHub, UpdateState, VersionInfo } from '../src/proto';
import { Nodes } from '../src/screens/Nodes';
import { clock } from '../src/ui/format';
import { feed, freshStore, node } from './helpers';

const ver = (version: string, git = 'abcdef1234', proto = 3): VersionInfo => ({ version, git, proto, os: 'linux', arch: 'x86_64' });
let sent: UiToHub[];
beforeEach(() => {
  sent = freshStore();
  feed({ type: 'node_update', ...node('n1', 'alpha') }, { type: 'node_update', ...node('n2', 'beta') });
});
const status = (n: string, s: UpdateState) => feed({ type: 'update_status', node: n, status: s });

describe('Nodes', () => {
  it('empty state', () => {
    freshStore();
    render(<Nodes />);
    expect(screen.getByText('No nodes yet')).toBeTruthy();
    expect(screen.getByText('Hub version unknown')).toBeTruthy();
  });

  it('every UpdateState variant renders its label', () => {
    const cases: [UpdateState, string][] = [
      [{ state: 'up_to_date', checked_ms: 5_000 }, `Up to date (checked ${clock(5_000)})`],
      [{ state: 'available', version: '9.9.9' }, 'Available: 9.9.9'],
      [{ state: 'checking' }, 'Checking...'],
      [{ state: 'downloading' }, 'Downloading...'],
      [{ state: 'verifying' }, 'Verifying...'],
      [{ state: 'installing' }, 'Installing...'],
      [{ state: 'restarting' }, 'Restarting...'],
      [{ state: 'failed', message: 'sha256 mismatch: expected abc got def, something long' }, 'Failed: sha256 mismatch: expected abc got def, something long'],
    ];
    for (const [st, label] of cases) {
      freshStore();
      feed({ type: 'node_update', ...node('n1', 'alpha') });
      status('n1', st);
      const { unmount } = render(<Nodes />);
      expect(screen.getByText(label), label).toBeTruthy();
      unmount();
    }
  });

  it('not checked / unknown platform before any data', () => {
    render(<Nodes />);
    expect(screen.getAllByText('Not checked')).toHaveLength(2);
    expect(screen.getAllByText('unknown platform')).toHaveLength(2);
  });

  it('Check sends check_update for that node; disabled while working or offline', () => {
    status('n2', { state: 'downloading' });
    feed({ type: 'node_update', ...node('n3', 'gamma', [], 'Offline') });
    render(<Nodes />);
    const checks = screen.getAllByRole('button', { name: 'Check' }) as HTMLButtonElement[];
    expect(checks.map((b) => b.disabled)).toEqual([false, true, true]);
    fireEvent.click(checks[0]!);
    expect(sent).toEqual([{ type: 'check_update', node: 'n1' }]);
  });

  it('Update needs two clicks and targets the node; only shown when available', () => {
    status('n2', { state: 'available', version: '2.0.0' });
    render(<Nodes />);
    expect(screen.getAllByRole('button', { name: 'Update' })).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Update' }));
    expect(sent).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Restarts the node: confirm' }));
    expect(sent).toEqual([{ type: 'update_node', node: 'n2' }]);
  });

  it('Check all and Update all (two clicks, disabled without updates)', () => {
    const { rerender } = render(<Nodes />);
    expect((screen.getByRole('button', { name: 'Update all' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Check all' }));
    expect(sent).toEqual([{ type: 'check_update', node: null }]);
    status('n1', { state: 'available', version: '2.0.0' });
    rerender(<Nodes />);
    fireEvent.click(screen.getByRole('button', { name: 'Update all' }));
    expect(sent).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Restarts every updated node: confirm' }));
    expect(sent[1]).toEqual({ type: 'update_node', node: null });
  });

  it('shows hub version and mismatch markers', () => {
    feed({ type: 'hub_version', ...ver('1.0.0') },
      { type: 'node_version', node: 'n1', info: ver('1.0.0') },
      { type: 'node_version', node: 'n2', info: ver('0.9.0') });
    render(<Nodes />);
    expect(screen.getByText('Hub 1.0.0 (abcdef1), protocol 3, linux x86_64')).toBeTruthy();
    expect(screen.getAllByText('differs from hub')).toHaveLength(1);
    feed({ type: 'node_version', node: 'n1', info: ver('1.0.0', 'abcdef1234', 2) });
    const { unmount } = render(<Nodes />);
    expect(screen.getAllByText('protocol too old')).toHaveLength(1);
    unmount();
  });

  it('going offline clears update and version info', () => {
    feed({ type: 'node_version', node: 'n1', info: ver('1.0.0') });
    status('n1', { state: 'available', version: '2.0.0' });
    feed({ type: 'node_update', ...node('n1', 'alpha', [], 'Offline') });
    render(<Nodes />);
    expect(screen.queryByText('Available: 2.0.0')).toBeNull();
    expect(screen.queryByText(/1\.0\.0 \(/)).toBeNull();
    expect(screen.getAllByText('unknown platform')).toHaveLength(2);
  });
});
