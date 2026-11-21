import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiToHub } from '../src/proto';
import { crumbs, LIST_TIMEOUT_MS } from '../src/screens/FolderPicker';
import { profileKey, SpawnDialog } from '../src/screens/SpawnDialog';
import { agentIdFromPrompt, store } from '../src/state';
import { blocker, feed, freshStore, node, ofType } from './helpers';

let sent: UiToHub[];
const onClose = vi.fn();
beforeEach(() => {
  sent = freshStore();
  onClose.mockClear();
  feed({ type: 'node_update', ...node('n1', 'alpha') }, { type: 'node_update', ...node('n2', 'beta') });
});
afterEach(() => vi.useRealTimers());
const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });
const startBtn = () => screen.getByRole('button', { name: /^(Start agent|Starting|Add to queue|Adding)/ }) as HTMLButtonElement;
const version = (n: string, v: string) => ({ type: 'node_version', node: n, info: { version: v, git: 'g', proto: 1, os: 'linux', arch: 'x86_64' } }) as const;

/** Open the dialog and pick a folder by typing its path (nodes that cannot list folders). */
function withFolder(path = '/srv/p', nodeName = 'alpha') {
  const r = render(<SpawnDialog onClose={onClose} />);
  fireEvent.click(screen.getByRole('button', { name: /^Where/ }));
  fireEvent.click(screen.getByRole('radio', { name: 'Folder on a node' }));
  const select = screen.getByLabelText('Node') as HTMLSelectElement;
  fireEvent.change(select, { target: { value: [...select.options].find((o) => o.text === nodeName)!.value } });
  type('Path', path);
  fireEvent.click(screen.getByRole('button', { name: 'Use this folder' }));
  return r;
}
const fill = () => {
  type('What should it do?', 'do it');
  type('Agent id', 'worker-1');
};

describe('agent id from the prompt', () => {
  it('slugs the first words, avoids taken ids and falls back for prompts without letters', () => {
    expect(agentIdFromPrompt('Fix the login bug, then add tests', new Set())).toBe('fix-the-login-bug');
    expect(agentIdFromPrompt('Fix the login bug', new Set(['fix-the-login-bug', 'fix-the-login-bug-2']))).toBe('fix-the-login-bug-3');
    expect(agentIdFromPrompt('Ünïcödé café', new Set())).toBe('unicode-cafe');
    expect(agentIdFromPrompt('🚀 中文', new Set())).toBe('agent');
    expect(agentIdFromPrompt('', new Set(['agent']))).toBe('agent-2');
    expect(agentIdFromPrompt('x'.repeat(100), new Set()).length).toBeLessThanOrEqual(40);
  });

  it('follows the prompt until edited, never reuses a running id, opens More options for a bad one', () => {
    feed({ type: 'node_update', ...node('n1', 'alpha', ['update-docs']) });
    render(<SpawnDialog onClose={onClose} />);
    type('What should it do?', 'Update docs please');
    expect((screen.getByLabelText('Agent id') as HTMLInputElement).value).toBe('update-docs-please');
    type('What should it do?', 'Update docs');
    expect((screen.getByLabelText('Agent id') as HTMLInputElement).value).toBe('update-docs-2');
    expect(screen.queryByText(/Use 1-64 characters/)).toBeNull();
    type('Agent id', 'Bad Name');
    expect(screen.getByText('Use 1-64 characters: lowercase letters, digits and "-".')).toBeTruthy();
    expect((document.querySelector('details') as HTMLDetailsElement).open).toBe(true);
    type('What should it do?', 'something else');
    expect((screen.getByLabelText('Agent id') as HTMLInputElement).value).toBe('Bad Name');
    type('Agent id', 'ok-1');
    expect(screen.queryByText(/Use 1-64 characters/)).toBeNull();
  });
});

describe('SpawnDialog', () => {
  it('profileKey', () => {
    expect(profileKey('/usr/bin/claude', ' ')).toBe('claude');
    expect(profileKey('claude', '~/.c')).toBe('claude@~/.c');
    expect(profileKey(' ', '')).toBeNull();
  });

  it('an empty prompt sends nothing and says why', () => {
    withFolder();
    fireEvent.click(startBtn());
    expect(sent).toEqual([]);
    expect(store.state.value.toasts.at(-1)?.text).toContain('Prompt is empty');
  });

  it('a folder pins Run on to its node and spawns there with the exact frame', () => {
    withFolder('/srv/p', 'beta');
    expect(screen.getByRole('button', { name: /^Where/ }).textContent).toContain('/srv/p on beta');
    expect((screen.getByLabelText('Run on') as HTMLSelectElement).value).toBe('n2');
    fill();
    fireEvent.click(startBtn());
    expect(sent).toEqual([{
      type: 'spawn', target: { Node: 'n2' },
      spec: { agent_id: 'worker-1', profile: { binary: 'claude', args: [], config_dir: null }, workdir: '/srv/p', prompt: 'do it', env: {} },
    }]);
    expect(startBtn().textContent).toBe('Starting...');
    expect(startBtn().disabled).toBe(true);
  });

  it('another node clears the folder', () => {
    withFolder();
    fireEvent.change(screen.getByLabelText('Run on'), { target: { value: 'n2' } });
    expect(screen.getByRole('button', { name: /^Where/ }).textContent).toContain('Choose a repository or a folder');
    expect(startBtn().disabled).toBe(true);
  });

  it('custom profile and config folder (folder spawns only)', () => {
    withFolder();
    fireEvent.change(screen.getByLabelText('Agent'), { target: { value: 'custom' } });
    type('Binary', '/opt/bin/my-agent');
    type('Config folder (CLAUDE_CONFIG_DIR)', '~/.cfg');
    fill();
    fireEvent.click(startBtn());
    expect(sent).toEqual([{
      type: 'spawn', target: { Node: 'n1' },
      spec: { agent_id: 'worker-1', profile: { binary: '/opt/bin/my-agent', args: [], config_dir: '~/.cfg' }, workdir: '/srv/p', prompt: 'do it', env: {} },
    }]);
  });

  it('closes and selects the agent once its status shows up', () => {
    withFolder();
    fill();
    fireEvent.click(startBtn());
    act(() => feed({ type: 'agent_status', agent: 'worker-1', node: 'n1', status: 'Starting' }, { type: 'node_update', ...node('n1', 'alpha', ['worker-1']) }));
    expect(sent.some((f) => f.type === 'attach')).toBe(false); // no session while starting
    expect(store.state.value.selected).toBe('worker-1');
    act(() => feed({ type: 'agent_status', agent: 'worker-1', node: 'n1', status: 'Running' }));
    expect(sent.at(-1)).toEqual({ type: 'attach', agent: 'worker-1' });
    expect(onClose).toHaveBeenCalled();
  });

  it('closes without attaching when the agent is already Failed', () => {
    withFolder();
    fill();
    fireEvent.click(startBtn());
    act(() => feed({ type: 'agent_status', agent: 'worker-1', node: 'n1', status: 'Failed' }, { type: 'node_update', ...node('n1', 'alpha', ['worker-1']) }));
    expect(sent.some((f) => f.type === 'attach')).toBe(false);
    expect(onClose).toHaveBeenCalled();
  });

  it('a blocked node+profile shows the warning and disables Start; a free node skips it', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1', { detail: 'log in first' }) });
    render(<SpawnDialog onClose={onClose} />);
    const runOn = screen.getByLabelText('Run on') as HTMLSelectElement;
    expect([...runOn.options].map((o) => o.text)).toEqual(['A free node', 'alpha - login required', 'beta']);
    fireEvent.change(runOn, { target: { value: 'n1' } });
    expect(screen.getByRole('alert').textContent).toContain('Login required on n1 (claude): log in first');
    expect(startBtn().disabled).toBe(true);
    fireEvent.change(runOn, { target: { value: '' } });
    expect(screen.getByText('A free node skips n1 because of blockers for claude.')).toBeTruthy();
  });

  it('A free node is blocked when every idle node is blocked; another agent is not', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1') }, { type: 'blocker_opened', ...blocker('b2', 'n2') });
    withFolder();
    fireEvent.change(screen.getByLabelText('Run on'), { target: { value: '' } });
    expect(screen.getByRole('alert').textContent).toContain('Resolve it in the Inbox first');
    fireEvent.change(screen.getByLabelText('Agent'), { target: { value: 'antigravity' } });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('a hub error after submit shows inline and lets you retry', () => {
    withFolder();
    fill();
    fireEvent.click(startBtn());
    act(() => feed({ type: 'error', message: 'no node can take this agent' }));
    expect(screen.getByText('no node can take this agent')).toBeTruthy();
    expect(startBtn().textContent).toBe('Start agent');
    expect(startBtn().disabled).toBe(false);
  });
});

describe('folder browser (nodes from 0.1.12)', () => {
  const openFolders = () => {
    render(<SpawnDialog onClose={onClose} />);
    fireEvent.click(screen.getByRole('button', { name: /^Where/ }));
    fireEvent.click(screen.getByRole('radio', { name: 'Folder on a node' }));
  };

  it('lists home, opens a folder, picks one by its absolute path; git folders show their branch', () => {
    feed(version('n1', '0.1.12'), { type: 'node_repos', node: 'n1', repos: [{ full_name: 'me/ulak', path: '/home/me/ulak', branch: 'main', clean: true, ahead: null, behind: null, last_fetch_ms: null, in_use: false, size_bytes: null }] } as never);
    openFolders();
    expect(ofType(sent, 'list_dir')).toEqual([{ type: 'list_dir', node: 'n1', path: '~' }]);
    expect(screen.getByText('Loading folders...')).toBeTruthy();
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '~', path: '/home/me', dirs: ['notes', 'ulak'], error: null } }));
    const list = within(screen.getByRole('list', { name: 'Folders in /home/me' }));
    expect(list.getByText('ulak').closest('button')!.textContent).toContain('git · main');
    fireEvent.click(list.getByRole('button', { name: 'Open notes' }));
    expect(ofType(sent, 'list_dir').at(-1)).toEqual({ type: 'list_dir', node: 'n1', path: '/home/me/notes' });
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '/home/me/notes', path: '/home/me/notes', dirs: [], error: null } }));
    expect(screen.getByText('No sub-folders.')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'me' }));
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '/home/me', path: '/home/me', dirs: ['notes', 'ulak'], error: null } }));
    fireEvent.click(screen.getByText('ulak').closest('button')!);
    fireEvent.click(screen.getByRole('button', { name: 'Use this folder' }));
    expect(screen.getByRole('button', { name: /^Where/ }).textContent).toContain('/home/me/ulak on alpha');
  });

  it('shows a refusal, and says so when the node does not answer', () => {
    vi.useFakeTimers();
    feed(version('n1', '0.1.12'));
    openFolders();
    act(() => void vi.advanceTimersByTime(LIST_TIMEOUT_MS));
    expect(screen.getByRole('alert').textContent).toContain('alpha did not answer');
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '~', path: '', dirs: [], error: 'only folders under your home or a repo root can be browsed' } }));
    expect(screen.getByRole('alert').textContent).toContain('only folders under your home');
  });

  it('a refused folder offers the way back home', () => {
    feed(version('n1', '0.1.12'));
    openFolders();
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '~', path: '/home/me', dirs: [], error: null } }));
    fireEvent.click(screen.getByRole('button', { name: 'home' }));
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '/home', path: '', dirs: [], error: 'only folders under your home or a repo root can be browsed' } }));
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Go to home folder' }));
    expect(screen.getByRole('list', { name: 'Folders in /home/me' })).toBeTruthy(); // cached answer for ~
  });

  it('reopening starts in the chosen folder and keeps it; a refreshed listing does not undo a row click', () => {
    feed(version('n1', '0.1.12'));
    openFolders();
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '~', path: '/home/me', dirs: ['notes', 'ulak'], error: null } }));
    fireEvent.click(screen.getByText('ulak').closest('button')!);
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '~', path: '/home/me', dirs: ['notes', 'ulak'], error: null } }));
    expect((screen.getByRole('textbox', { name: 'Path' }) as HTMLInputElement).value).toBe('/home/me/ulak');
    fireEvent.click(screen.getByRole('button', { name: 'Use this folder' }));
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: /^Where/ }));
    fireEvent.click(screen.getByRole('radio', { name: 'Folder on a node' }));
    expect(ofType(sent, 'list_dir')).toEqual([{ type: 'list_dir', node: 'n1', path: '/home/me/ulak' }]);
    act(() => feed({ type: 'dir_listing', node: 'n1', listing: { requested: '/home/me/ulak', path: '/home/me/ulak', dirs: [], error: null } }));
    expect((screen.getByRole('textbox', { name: 'Path' }) as HTMLInputElement).value).toBe('/home/me/ulak');
  });

  it('picks the first node when nodes arrive after the picker opens', () => {
    sent = freshStore();
    openFolders();
    act(() => feed({ type: 'node_update', ...node('n1', 'alpha') }));
    type('Path', '/srv/p');
    fireEvent.click(screen.getByRole('button', { name: 'Use this folder' }));
    expect(screen.getByRole('button', { name: /^Where/ }).textContent).toContain('/srv/p on alpha');
  });

  it('older nodes: no request, a typed path or a known project', () => {
    openFolders();
    expect(ofType(sent, 'list_dir')).toEqual([]);
    expect(screen.getByText(/cannot list its folders yet/)).toBeTruthy();
  });

  it('breadcrumbs', () => {
    expect(crumbs('/home/me/src')).toEqual([['/', '/'], ['home', '/home'], ['me', '/home/me'], ['src', '/home/me/src']]);
    expect(crumbs('/')).toEqual([['/', '/']]);
  });
});
