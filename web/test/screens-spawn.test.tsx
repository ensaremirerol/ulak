import { act, fireEvent, render, screen } from '@testing-library/preact';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiToHub } from '../src/proto';
import { store } from '../src/state';
import { profileKey, SpawnDialog } from '../src/screens/SpawnDialog';
import { blocker, feed, freshStore, node } from './helpers';

const flags = vi.hoisted(() => ({ repo: false, queue: false }));
vi.mock('../src/screens/flags', () => ({
  get REPO_SOURCE_READY() { return flags.repo; },
  get QUEUE_READY() { return flags.queue; },
}));

let sent: UiToHub[];
const onClose = vi.fn();
beforeEach(() => {
  sent = freshStore();
  flags.repo = false;
  flags.queue = false;
  onClose.mockClear();
  feed({ type: 'node_update', ...node('n1', 'alpha') }, { type: 'node_update', ...node('n2', 'beta') });
});
const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });
const spawnBtn = () => screen.getByRole('button', { name: /^Spawn/ }) as HTMLButtonElement;
const fill = () => {
  type('Agent id', 'worker-1');
  type('Prompt', 'do it');
  type('Working directory', '/srv/p');
};

describe('SpawnDialog', () => {
  it('profileKey', () => {
    expect(profileKey('/usr/bin/claude', ' ')).toBe('claude');
    expect(profileKey('claude', '~/.c')).toBe('claude@~/.c');
    expect(profileKey(' ', '')).toBeNull();
  });

  it('agent id validation message appears only for invalid ids', () => {
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.queryByText(/Use 1-64 characters/)).toBeNull();
    type('Agent id', 'Bad Name');
    expect(screen.getByText('Use 1-64 characters: lowercase letters, digits and "-".')).toBeTruthy();
    type('Agent id', 'x'.repeat(65));
    expect(screen.getByText(/Use 1-64 characters/)).toBeTruthy();
    type('Agent id', 'ok-1');
    expect(screen.queryByText(/Use 1-64 characters/)).toBeNull();
  });

  it('invalid or incomplete form sends nothing and toasts the reason', () => {
    render(<SpawnDialog onClose={onClose} />);
    type('Agent id', 'w');
    fireEvent.click(spawnBtn());
    expect(sent).toEqual([]);
    expect(store.state.value.toasts.at(-1)?.text).toContain('Prompt is empty');
  });

  it('valid spawn (Auto) sends the exact frame', () => {
    render(<SpawnDialog onClose={onClose} />);
    fill();
    fireEvent.click(spawnBtn());
    expect(sent).toEqual([{
      type: 'spawn', target: 'Auto',
      spec: { agent_id: 'worker-1', profile: { binary: 'claude', args: [], config_dir: null }, workdir: '/srv/p', prompt: 'do it', env: {} },
    }]);
    expect(spawnBtn().textContent).toBe('Spawning...');
    expect(spawnBtn().disabled).toBe(true);
  });

  it('custom profile, config dir and chosen node', () => {
    render(<SpawnDialog onClose={onClose} />);
    fireEvent.click(screen.getByRole('radio', { name: 'custom' }));
    type('Binary', '/opt/bin/my-agent');
    type('CLAUDE_CONFIG_DIR (optional)', '~/.cfg');
    fill();
    fireEvent.click(screen.getByRole('button', { name: 'beta' }));
    fireEvent.click(spawnBtn());
    expect(sent).toEqual([{
      type: 'spawn', target: { Node: 'n2' },
      spec: { agent_id: 'worker-1', profile: { binary: '/opt/bin/my-agent', args: [], config_dir: '~/.cfg' }, workdir: '/srv/p', prompt: 'do it', env: {} },
    }]);
  });

  it('closes and selects the agent once its status shows up', () => {
    render(<SpawnDialog onClose={onClose} />);
    fill();
    fireEvent.click(spawnBtn());
    act(() => feed({ type: 'agent_status', agent: 'worker-1', node: 'n1', status: 'Starting' }));
    expect(sent.at(-1)).toEqual({ type: 'attach', agent: 'worker-1' });
    expect(onClose).toHaveBeenCalled();
  });

  it('blocked node+profile shows the warning and disables Spawn', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1', { detail: 'log in first' }) });
    render(<SpawnDialog onClose={onClose} />);
    fill();
    fireEvent.click(screen.getByRole('button', { name: 'alpha - login required' }));
    expect(screen.getByRole('alert').textContent).toContain('Login required on n1 (claude): log in first');
    expect(spawnBtn().disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: /^beta/ }));
    expect(spawnBtn().disabled).toBe(false);
    fireEvent.click(screen.getByRole('button', { name: 'Auto' }));
    expect(screen.getByText('Auto skips n1 because of blockers for claude.')).toBeTruthy();
    expect(spawnBtn().disabled).toBe(false);
  });

  it('Auto is blocked when every idle node is blocked', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1') }, { type: 'blocker_opened', ...blocker('b2', 'n2') });
    render(<SpawnDialog onClose={onClose} />);
    expect(spawnBtn().disabled).toBe(true);
    fireEvent.click(screen.getByRole('radio', { name: 'antigravity' }));
    expect(spawnBtn().disabled).toBe(false);
  });

  it('hub error frame shows inline after submit', () => {
    render(<SpawnDialog onClose={onClose} />);
    fill();
    fireEvent.click(spawnBtn());
    act(() => feed({ type: 'error', message: 'no node can take this agent' }));
    expect(screen.getByText('no node can take this agent')).toBeTruthy();
    expect(spawnBtn().textContent).toBe('Spawn');
    expect(spawnBtn().disabled).toBe(false);
  });

  it('repo source disabled while REPO_SOURCE_READY is false; enabled and blocking spawn when true', () => {
    const { unmount } = render(<SpawnDialog onClose={onClose} />);
    const repo = screen.getByRole('radio', { name: 'Repository (coming next)' }) as HTMLButtonElement;
    expect(repo.disabled).toBe(true);
    expect(screen.queryByLabelText('Repository')).toBeNull();
    expect(screen.getByLabelText('Working directory')).toBeTruthy();
    unmount();
    flags.repo = true;
    render(<SpawnDialog onClose={onClose} />);
    expect((screen.getByRole('radio', { name: 'Repository' }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByLabelText('Branch')).toBeTruthy();
    expect(screen.queryByLabelText('Working directory')).toBeNull();
    expect(spawnBtn().disabled).toBe(true);
    fireEvent.click(screen.getByRole('radio', { name: 'Folder on the node' }));
    expect(spawnBtn().disabled).toBe(false);
  });

  it('queue note hidden while QUEUE_READY is false, shown when true and no node is free', () => {
    feed({ type: 'node_update', ...node('n1', 'alpha', [], { Busy: 'Load' }) }, { type: 'node_update', ...node('n2', 'beta', [], { Busy: 'Load' }) });
    const { unmount } = render(<SpawnDialog onClose={onClose} />);
    expect(screen.queryByText(/wait in the queue/)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Add to queue' })).toBeNull();
    unmount();
    flags.queue = true;
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.getByText('No node is free: this agent will wait in the queue.')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Add to queue' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('queue note stays hidden when flag is on but a node is idle', () => {
    flags.queue = true;
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.queryByText(/wait in the queue/)).toBeNull();
  });
});
