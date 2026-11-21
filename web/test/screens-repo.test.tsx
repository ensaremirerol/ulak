import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { HubToUi, QueuedSpawn, UiToHub } from '../src/proto';
import { Agents } from '../src/screens/Agents';
import { SpawnDialog } from '../src/screens/SpawnDialog';
import { Settings } from '../src/screens/Settings';
import { store } from '../src/state';
import { Shell } from '../src/ui/Shell';
import { feed, freshStore, node, ofType } from './helpers';

vi.mock('../src/ui/Terminal', () => ({ Terminal: () => <div /> }));

const cred = { id: 'cred-1', name: 'work', kind: 'github' as const, login: 'octocat', created_ms: 1_700_000_005_000 };
const repo = (n: string, over = {}) => ({ full_name: `octocat/${n}`, private: false, default_branch: 'main', pushed_at_ms: 1_000_000 - 120_000, description: `about ${n}`, ...over });
const reposFrame = (repos: ReturnType<typeof repo>[], has_more = false, query: string | null = null): HubToUi => ({ type: 'repos', credential: 'cred-1', query, repos, has_more });
const item = (id: string, over: Partial<QueuedSpawn> = {}): QueuedSpawn => ({
  id, agent: `agent-${id}`, target: 'Auto', profile: 'claude', prompt_preview: 'x', created_ms: 1_000_000 - 90_000, state: 'Queued',
  source: { kind: 'repo', repo: 'octocat/hello-world', branch: 'main', credential: 'cred-1', allow_push: false }, ...over,
});
const onClose = vi.fn();
let sent: UiToHub[];
beforeEach(() => {
  vi.useFakeTimers();
  sent = freshStore();
  feed({ type: 'node_update', ...node('n1', 'alpha') });
});
afterEach(() => vi.useRealTimers());
const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });

describe('SpawnDialog: repository source', () => {
  it('opens on Repository and asks for a token when there is no credential; the token is write-only', () => {
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.getByRole('radio', { name: 'Repository' }).getAttribute('aria-checked')).toBe('true');
    const link = screen.getByRole('link', { name: /Contents: read/ });
    expect(link.getAttribute('href')).toBe('https://github.com/settings/personal-access-tokens/new');
    expect(link.getAttribute('rel')).toBe('noopener noreferrer');
    expect((screen.getByLabelText('GitHub token') as HTMLInputElement).type).toBe('password');
    type('Token name', 'work');
    type('GitHub token', 'ghp_secret123');
    fireEvent.click(screen.getByRole('button', { name: 'Add token' }));
    expect(sent).toEqual([{ type: 'add_credential', name: 'work', token: 'ghp_secret123' }]);
    expect((screen.getByLabelText('GitHub token') as HTMLInputElement).value).toBe('');
    act(() => feed({ type: 'credentials', credentials: [cred] }));
    expect(screen.getByText('GitHub · octocat')).toBeTruthy();
    expect(document.body.innerHTML).not.toContain('ghp_secret123');
    expect(JSON.stringify(store.state.value)).not.toContain('ghp_secret123');
  });

  function open() {
    feed({ type: 'credentials', credentials: [cred] });
    render(<SpawnDialog onClose={onClose} />);
  }

  it('lists repos, debounces search by 300 ms, loads more', () => {
    open();
    expect(sent).toEqual([{ type: 'list_repos', credential: 'cred-1', query: null, page: 1 }]);
    expect(screen.getByText('Loading repositories...')).toBeTruthy();
    act(() => feed(reposFrame([repo('one', { private: true })], true)));
    const list = within(screen.getByRole('list', { name: 'Repositories' }));
    expect(list.getByText('octocat/one').closest('button')!.textContent).toContain('Private · pushed 2 min ago');
    expect(list.getByText('about one')).toBeTruthy();
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Load more' }));
    expect(sent).toEqual([{ type: 'list_repos', credential: 'cred-1', query: null, page: 2 }]);
    act(() => feed(reposFrame([repo('two')], false)));
    expect(screen.getByText('octocat/one')).toBeTruthy();
    expect(screen.getByText('octocat/two')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Load more' })).toBeNull();
    sent.length = 0;
    type('Search repositories', 'hel');
    type('Search repositories', 'hello');
    act(() => void vi.advanceTimersByTime(299));
    expect(sent).toEqual([]);
    act(() => void vi.advanceTimersByTime(1));
    expect(sent).toEqual([{ type: 'list_repos', credential: 'cred-1', query: 'hello', page: 1 }]);
    act(() => feed(reposFrame([], false, 'hello')));
    expect(screen.getByText('No repositories found.')).toBeTruthy();
  });

  it('shows a hub error inline while loading repos', () => {
    open();
    act(() => feed({ type: 'error', message: 'GitHub said no' }));
    expect(screen.getByRole('alert').textContent).toBe('GitHub said no');
  });

  it('selecting a repo requests branches, preselects the default, and spawns with the exact source', () => {
    open();
    act(() => feed(reposFrame([repo('hello-world')])));
    sent.length = 0;
    fireEvent.click(screen.getByText('octocat/hello-world'));
    expect(sent).toEqual([{ type: 'list_branches', credential: 'cred-1', repo: 'octocat/hello-world' }]);
    act(() => feed({ type: 'branches', credential: 'cred-1', repo: 'octocat/hello-world', branches: ['dev', 'main'], default_branch: 'main' }));
    expect((screen.getByLabelText('Branch') as HTMLSelectElement).value).toBe('main');
    const push = screen.getByRole('switch', { name: 'Allow this agent to push' }) as HTMLInputElement;
    expect(push.checked).toBe(false);
    expect(screen.queryByText(/hold your GitHub token/)).toBeNull();
    type('Agent id', 'reviewer-2');
    type('Prompt', 'fix the bug');
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Spawn' }));
    expect(sent).toEqual([{
      type: 'spawn', target: 'Auto',
      spec: { agent_id: 'reviewer-2', env: {}, profile: { args: [], binary: 'claude', config_dir: null }, prompt: 'fix the bug', workdir: '',
        source: { kind: 'repo', repo: 'octocat/hello-world', branch: 'main', credential: 'cred-1', allow_push: false } },
    }]);
    fireEvent.click(push);
    expect(screen.getByText(/hold your GitHub token/)).toBeTruthy();
  });

  it('Spawn stays disabled until a repo and branch are chosen; folder stays available', () => {
    open();
    expect((screen.getByRole('button', { name: 'Spawn' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('radio', { name: 'Folder on the node' }));
    expect(screen.getByLabelText('Working directory')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Spawn' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('Remove is a two-click delete_credential', () => {
    open();
    fireEvent.click(screen.getByRole('button', { name: 'Remove' }));
    expect(ofType(sent, 'delete_credential')).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Click again to remove' }));
    expect(ofType(sent, 'delete_credential')).toEqual([{ type: 'delete_credential', id: 'cred-1' }]);
  });
});

describe('queue', () => {
  it('normal state: no notice, button says Spawn', () => {
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.queryByText(/wait in the queue/)).toBeNull();
    expect(screen.getByRole('button', { name: 'Spawn' })).toBeTruthy();
  });

  it('no eligible node: notice and Add to queue', () => {
    feed({ type: 'node_update', ...node('n1', 'alpha', [], { Busy: 'Load' }) });
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.getByText('No node is free right now. This agent will wait in the queue and start by itself when one is.')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Add to queue' })).toBeTruthy();
  });

  it('a node blocked for the profile is not eligible', () => {
    feed({ type: 'blocker_opened', id: 'b', node: 'n2', profile: 'claude', kind: 'Login', agent: 'a', detail: 'd', at_ms: 1 }, { type: 'node_update', ...node('n2', 'beta', [], { Busy: 'Load' }) });
    feed({ type: 'node_update', ...node('n1', 'alpha', [], 'Offline') });
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.getByRole('button', { name: 'Add to queue' })).toBeTruthy();
  });

  it('Agents shows the queue, updates live, and Cancel needs two clicks', () => {
    feed({ type: 'queue', items: [item('q-1'), item('q-2', { source: { kind: 'folder', path: '/srv/p' }, target: { Node: 'n1' }, state: { Failed: 'credential cred-1 is gone' } })] });
    render(<Agents />);
    const q = within(screen.getByRole('list', { name: 'Queue' }));
    expect(q.getByText('agent-q-1')).toBeTruthy();
    expect(q.getByText(/octocat\/hello-world@main · Auto · 1 min ago/)).toBeTruthy();
    expect(q.getByText(/\/srv\/p · node n1/)).toBeTruthy();
    expect(q.getByText('credential cred-1 is gone')).toBeTruthy();
    expect(screen.getByText('Waiting for a node · 2')).toBeTruthy();
    act(() => feed({ type: 'queue_changed', item: item('q-3') }));
    expect(screen.getByText('Waiting for a node · 3')).toBeTruthy();
    fireEvent.click(q.getAllByRole('button', { name: 'Cancel' })[0]!);
    expect(ofType(sent, 'cancel_queued')).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Click again to cancel' }));
    expect(ofType(sent, 'cancel_queued')).toEqual([{ type: 'cancel_queued', id: 'q-1' }]);
    act(() => feed({ type: 'queue_removed', id: 'q-1' }));
    expect(screen.getByText('Waiting for a node · 2')).toBeTruthy();
  });

  it('the Agents nav item shows the queue count', () => {
    feed({ type: 'queue', items: [item('q-1'), item('q-2')] });
    render(<Shell><div /></Shell>);
    expect(screen.getByLabelText('2 waiting').closest('a')!.textContent).toContain('Agents');
  });

  it('a reconnect resets statuses and queue; resync asks for credentials and queue', () => {
    feed({ type: 'queue', items: [item('q-1')] }, { type: 'agent_status', agent: 'a', node: 'n1', status: 'Running' });
    store.dispatch({ kind: 'conn', state: 'reconnecting' });
    store.dispatch({ kind: 'conn', state: 'open' });
    expect(store.state.value.queue).toEqual([]);
    expect(store.state.value.statuses).toEqual({});
    expect(store.resync().map((f) => f.type)).toEqual(['list_credentials', 'list_queue']);
  });
});

describe('Settings credentials', () => {
  it('lists credentials, Delete is two-click, Add sends the token and clears the field', () => {
    feed({ type: 'credentials', credentials: [cred] });
    render(<Settings />);
    expect(screen.getByText(/GitHub · octocat · added/)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    expect(sent).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Click again to delete' }));
    expect(sent).toEqual([{ type: 'delete_credential', id: 'cred-1' }]);
    fireEvent.click(screen.getByRole('button', { name: 'Add' }));
    type('Token name', 'second');
    type('GitHub token', 'ghp_other');
    fireEvent.click(screen.getByRole('button', { name: 'Add token' }));
    expect(sent[1]).toEqual({ type: 'add_credential', name: 'second', token: 'ghp_other' });
    expect(document.body.innerHTML).not.toContain('ghp_other');
  });
});
