import { act, fireEvent, render, screen } from '@testing-library/preact';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiToHub } from '../src/proto';
import { store } from '../src/state';
import { profileKey, SpawnDialog } from '../src/screens/SpawnDialog';
import { blocker, feed, freshStore, node } from './helpers';


let sent: UiToHub[];
const onClose = vi.fn();
beforeEach(() => {
  sent = freshStore();
  onClose.mockClear();
  feed({ type: 'node_update', ...node('n1', 'alpha') }, { type: 'node_update', ...node('n2', 'beta') });
});
const renderFolder = () => {
  const r = render(<SpawnDialog onClose={onClose} />);
  fireEvent.click(screen.getByRole('radio', { name: 'Folder on the node' }));
  return r;
};
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
    renderFolder();
    expect(screen.queryByText(/Use 1-64 characters/)).toBeNull();
    type('Agent id', 'Bad Name');
    expect(screen.getByText('Use 1-64 characters: lowercase letters, digits and "-".')).toBeTruthy();
    type('Agent id', 'x'.repeat(65));
    expect(screen.getByText(/Use 1-64 characters/)).toBeTruthy();
    type('Agent id', 'ok-1');
    expect(screen.queryByText(/Use 1-64 characters/)).toBeNull();
  });

  it('invalid or incomplete form sends nothing and toasts the reason', () => {
    renderFolder();
    type('Agent id', 'w');
    fireEvent.click(spawnBtn());
    expect(sent).toEqual([]);
    expect(store.state.value.toasts.at(-1)?.text).toContain('Prompt is empty');
  });

  it('valid spawn (Auto) sends the exact frame', () => {
    renderFolder();
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
    renderFolder();
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
    renderFolder();
    fill();
    fireEvent.click(spawnBtn());
    act(() => feed({ type: 'agent_status', agent: 'worker-1', node: 'n1', status: 'Starting' }));
    expect(sent.some((f) => f.type === 'attach')).toBe(false); // no session while starting
    expect(store.state.value.selected).toBe('worker-1');
    act(() => feed({ type: 'agent_status', agent: 'worker-1', node: 'n1', status: 'Running' }));
    expect(sent.at(-1)).toEqual({ type: 'attach', agent: 'worker-1' });
    expect(onClose).toHaveBeenCalled();
  });

  it('closes without attaching when the agent is already Failed', () => {
    renderFolder();
    fill();
    fireEvent.click(spawnBtn());
    act(() => feed({ type: 'agent_status', agent: 'worker-1', node: 'n1', status: 'Failed' }));
    expect(sent.some((f) => f.type === 'attach')).toBe(false);
    expect(onClose).toHaveBeenCalled();
  });

  it('blocked node+profile shows the warning and disables Spawn', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1', { detail: 'log in first' }) });
    renderFolder();
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
    renderFolder();
    expect(spawnBtn().disabled).toBe(true);
    fireEvent.click(screen.getByRole('radio', { name: 'antigravity' }));
    expect(spawnBtn().disabled).toBe(false);
  });

  it('hub error frame shows inline after submit', () => {
    renderFolder();
    fill();
    fireEvent.click(spawnBtn());
    act(() => feed({ type: 'error', message: 'no node can take this agent' }));
    expect(screen.getByText('no node can take this agent')).toBeTruthy();
    expect(spawnBtn().textContent).toBe('Spawn');
    expect(spawnBtn().disabled).toBe(false);
  });
});
