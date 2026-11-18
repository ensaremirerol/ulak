import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { CleanItem, ProfileInfo, RepoEntry, UiToHub } from '../src/proto';
import { Agents } from '../src/screens/Agents';
import { NodeDetail } from '../src/screens/NodeDetail';
import { SpawnDialog } from '../src/screens/SpawnDialog';
import { repoHint, repoRank, store } from '../src/state';
import { CONFIRM_MS } from '../src/ui/ConfirmButton';
import { feed, freshStore, node } from './helpers';

vi.mock('../src/ui/Terminal', () => ({ Terminal: () => <div /> }));

const prof = (name: string, over: Partial<ProfileInfo> = {}): ProfileInfo =>
  ({ name, binary: 'claude', config_dir: null, installed: true, login_blocked: false, ...over });
const entry = (full_name: string, over: Partial<RepoEntry> = {}): RepoEntry => ({
  full_name, path: `/w/a/${full_name.split('/')[1]}`, branch: 'main', clean: true, ahead: 0, behind: 0,
  last_fetch_ms: 1_000_000 - 120_000, in_use: false, managed: true, ...over,
});
const cred = { id: 'cred-1', name: 'work', kind: 'github' as const, login: 'octocat', created_ms: 1 };
const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });
const onClose = vi.fn();
let sent: UiToHub[];

beforeEach(() => {
  vi.useFakeTimers();
  localStorage.clear();
  sent = freshStore();
  feed({ type: 'node_update', ...node('n1', 'alpha') }, { type: 'node_update', ...node('n2', 'beta') });
});
afterEach(() => vi.useRealTimers());

describe('repo preference (mirrors the hub)', () => {
  it('rank and hint', () => {
    expect(repoRank(undefined, 'o/n')).toBe(2);
    expect(repoRank([entry('o/n')], 'o/n')).toBe(0);
    expect(repoRank([entry('o/n', { in_use: true })], 'o/n')).toBe(1);
    expect(repoRank([entry('o/n', { managed: false })], 'o/n')).toBe(1);
    const s = () => store.state.value;
    expect(repoHint(s(), 'o/n')).toBeNull();
    feed({ type: 'node_repos', node: 'n1', repos: [entry('o/n', { clean: false })] });
    expect(repoHint(s(), 'o/n')).toBe('Already on alpha (has changes)');
    feed({ type: 'node_repos', node: 'n2', repos: [entry('o/n')] });
    expect(repoHint(s(), 'o/n')).toBe('Already on beta (clean)');
  });
});

describe('SpawnDialog: profiles and fresh', () => {
  const folder = () => {
    render(<SpawnDialog onClose={onClose} />);
    fireEvent.click(screen.getByRole('radio', { name: 'Folder on the node' }));
    type('Agent id', 'w-1');
    type('Prompt', 'go');
    type('Working directory', '/srv/p');
  };

  it('picker is the union of node profiles with where each lives; legacy list without reports', () => {
    render(<SpawnDialog onClose={onClose} />);
    expect(within(screen.getByRole('radiogroup', { name: 'Profile' })).getAllByRole('radio').map((r) => r.textContent)).toEqual(['claude', 'antigravity', 'custom']);
    act(() => feed(
      { type: 'node_profiles', node: 'n1', profiles: [prof('claude-work', { config_dir: '/h/.cw' }), prof('antigravity', { installed: false })] },
      { type: 'node_profiles', node: 'n2', profiles: [prof('claude-work', { login_blocked: true })] },
    ));
    expect(within(screen.getByRole('radiogroup', { name: 'Profile' })).getAllByRole('radio').map((r) => r.textContent)).toEqual(['claude-work', 'antigravity', 'custom']);
    expect(screen.getByText('On alpha: config /h/.cw; beta: login required')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'beta - login required' })).toBeTruthy();
    fireEvent.click(screen.getByRole('radio', { name: 'antigravity' }));
    expect(screen.getByText('On alpha: not installed, installs on first spawn')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'beta - no antigravity' })).toBeTruthy();
    // An offline node's last report is not offered.
    act(() => feed({ type: 'node_update', ...node('n1', 'alpha', [], 'Offline') }));
    expect(within(screen.getByRole('radiogroup', { name: 'Profile' })).getAllByRole('radio').map((r) => r.textContent)).toEqual(['claude-work', 'custom']);
  });

  it('spawns with the chosen profile name, remembers it, hides the config dir under Advanced', () => {
    feed({ type: 'node_profiles', node: 'n1', profiles: [prof('claude-work'), prof('claude-personal')] });
    folder();
    expect(screen.getByLabelText('CLAUDE_CONFIG_DIR (optional)').closest('details')!.open).toBe(false);
    fireEvent.click(screen.getByRole('radio', { name: 'claude-personal' }));
    fireEvent.click(screen.getByRole('button', { name: 'Spawn' }));
    expect(sent).toEqual([{
      type: 'spawn', target: 'Auto',
      spec: { agent_id: 'w-1', profile: { binary: 'claude-personal', args: [], config_dir: null }, workdir: '/srv/p', prompt: 'go', env: {} },
    }]);
    expect(localStorage.getItem('ulak.spawn.profile')).toBe('claude-personal');
  });

  it('defaults to the last used profile when a node still offers it', () => {
    localStorage.setItem('ulak.spawn.profile', 'claude-personal');
    feed({ type: 'node_profiles', node: 'n1', profiles: [prof('claude-work'), prof('claude-personal')] });
    render(<SpawnDialog onClose={onClose} />);
    expect(screen.getByRole('radio', { name: 'claude-personal' }).getAttribute('aria-checked')).toBe('true');
  });

  it('a node lacking the profile is not a target: the spawn queues', () => {
    feed(
      { type: 'node_update', ...node('n2', 'beta', [], { Busy: 'Load' }) },
      { type: 'node_profiles', node: 'n1', profiles: [prof('claude-work')] },
      { type: 'node_profiles', node: 'n2', profiles: [prof('claude-solo')] },
    );
    folder();
    expect(screen.getByRole('button', { name: 'Spawn' })).toBeTruthy();
    fireEvent.click(screen.getByRole('radio', { name: 'claude-solo' }));
    expect(screen.getByRole('button', { name: 'alpha - no claude-solo' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Add to queue' })).toBeTruthy();
  });

  it('agent id looks required and says so when missing', () => {
    folder();
    type('Agent id', '');
    const input = screen.getByLabelText('Agent id');
    expect(input.getAttribute('aria-required')).toBe('true');
    expect(screen.queryByText('Agent id is required.')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Spawn' }));
    expect(screen.getByText('Agent id is required.')).toBeTruthy();
    expect(sent).toEqual([]);
  });

  it('repo spawn: fresh switch (off by default) and the reuse hint', () => {
    feed({ type: 'credentials', credentials: [cred] }, { type: 'node_repos', node: 'n2', repos: [entry('octocat/hello-world')] });
    render(<SpawnDialog onClose={onClose} />);
    act(() => feed({ type: 'repos', credential: 'cred-1', query: null, has_more: false, repos: [{ full_name: 'octocat/hello-world', private: false, default_branch: 'main', pushed_at_ms: 1, description: '' }] }));
    fireEvent.click(screen.getByText('octocat/hello-world'));
    act(() => feed({ type: 'branches', credential: 'cred-1', repo: 'octocat/hello-world', branches: ['main'], default_branch: 'main' }));
    expect(screen.getByRole('status').textContent).toBe('Already on beta (clean)');
    const fresh = screen.getByRole('switch', { name: 'Fresh clone' }) as HTMLInputElement;
    expect(fresh.checked).toBe(false);
    expect(screen.getByText('Off: reuse an existing clone on the node when possible.')).toBeTruthy();
    expect(screen.queryByLabelText('CLAUDE_CONFIG_DIR (optional)')).toBeNull();
    type('Agent id', 'r-1');
    type('Prompt', 'go');
    const src = { kind: 'repo', repo: 'octocat/hello-world', branch: 'main', credential: 'cred-1', allow_push: false };
    const frame = (source: object) => ({
      type: 'spawn', target: 'Auto',
      spec: { agent_id: 'r-1', profile: { binary: 'claude', args: [], config_dir: null }, workdir: '', prompt: 'go', env: {}, source },
    });
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Spawn' }));
    expect(sent).toEqual([frame(src)]);
    act(() => feed({ type: 'error', message: 'nope' }));
    fireEvent.click(fresh);
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Spawn' }));
    expect(sent).toEqual([frame({ ...src, fresh: true })]);
  });
});

describe('Node screen: profiles, repositories, cleanup', () => {
  const removed: CleanItem[] = [{ path: '/w/a/hello-world', full_name: 'octocat/hello-world', reason: 'up to date with the remote (last fetch)' }];
  const kept: CleanItem[] = [{ path: '/home/u/code/other', full_name: null, reason: 'not under the work root' }];

  it('empty states', () => {
    render(<NodeDetail id="n1" />);
    const sec = within(screen.getByRole('region', { name: 'Repositories on alpha' }));
    expect(sec.getByText('Repositories (0)')).toBeTruthy();
    expect(sec.getByText('No repositories indexed on this node.')).toBeTruthy();
    expect(sec.queryByRole('status')).toBeNull();
  });

  it('lists profiles and repos with their state', () => {
    feed(
      { type: 'node_profiles', node: 'n1', profiles: [prof('claude-work', { config_dir: '/h/.cw' }), prof('antigravity', { installed: false, login_blocked: true })] },
      { type: 'node_repos', node: 'n1', repos: [entry('octocat/hello-world'), entry('octocat/dirty', { clean: false, ahead: 1, behind: 2, in_use: true, last_fetch_ms: null }), entry('me/ext', { managed: false, ahead: null, behind: null, branch: null })] },
    );
    render(<NodeDetail id="n1" />);
    const profiles = within(screen.getByRole('list', { name: 'Profiles on alpha' })).getAllByRole('listitem').map((li) => li.textContent);
    expect(profiles).toEqual(['claude-work/h/.cwInstalled', 'antigravitydefault configNot installedLogin required']);
    const rows = within(screen.getByRole('list', { name: 'Repositories of alpha' })).getAllByRole('listitem');
    expect(rows.map((li) => li.textContent)).toEqual([
      'octocat/hello-worldmainCleanin syncfetched 2 min agomanagedRemoveClick again to remove', // ConfirmButton keeps both labels (one hidden) for a stable width
      'octocat/dirtymainDirty1 ahead, 2 behindnever fetchedIn usemanaged',
      'me/extdetachedCleanno upstreamfetched 2 min agoexternal',
    ]);
  });

  it('preview, two-click clean up and per-repo remove send the exact frames; the report renders', () => {
    feed({ type: 'node_repos', node: 'n1', repos: [entry('octocat/hello-world')] });
    render(<NodeDetail id="n1" />);
    expect(sent).toEqual([{ type: 'node_stats', node: 'n1' }]);
    sent.length = 0;
    const sec = within(screen.getByRole('region', { name: 'Repositories on alpha' }));
    fireEvent.click(sec.getByRole('button', { name: 'Preview cleanup' }));
    expect(sent).toEqual([{ type: 'clean_node', node: 'n1', dry_run: true }]);
    act(() => feed({ type: 'clean_report', node: 'n1', dry_run: true, removed, kept }));
    const rep = within(sec.getByRole('status', { name: 'Cleanup report for alpha' }));
    expect(rep.getByText('Would remove (1)')).toBeTruthy();
    expect(rep.getByText('octocat/hello-world')).toBeTruthy();
    expect(rep.getByText('Kept (1)')).toBeTruthy();
    expect(rep.getByText('not under the work root')).toBeTruthy();
    sent.length = 0;
    fireEvent.click(sec.getByRole('button', { name: 'Clean up' }));
    expect(sent).toEqual([]);
    fireEvent.click(sec.getByRole('button', { name: 'Deletes unused clones: confirm' }));
    expect(sent).toEqual([{ type: 'clean_node', node: 'n1', dry_run: false }]);
    act(() => feed({ type: 'clean_report', node: 'n1', dry_run: false, removed, kept: [] }));
    expect(rep.getByText('Removed (1)')).toBeTruthy();
    sent.length = 0;
    fireEvent.click(sec.getByRole('button', { name: 'Remove' }));
    act(() => { vi.advanceTimersByTime(CONFIRM_MS + 1); });
    expect(sec.getByRole('button', { name: 'Remove' })).toBeTruthy();
    fireEvent.click(sec.getByRole('button', { name: 'Remove' }));
    fireEvent.click(sec.getByRole('button', { name: 'Click again to remove' }));
    expect(sent).toEqual([{ type: 'clean_repo', node: 'n1', path: '/w/a/hello-world' }]);
  });
});

describe('Agents: failed agents', () => {
  it('a failed agent the node never listed shows up without a reload', () => {
    render(<Agents />);
    act(() => feed({ type: 'agent_failed', agent: 'ghost-1', node: 'n1', reason: 'clone failed' }));
    expect(within(screen.getByRole('list', { name: 'Agents' })).getByText('ghost-1')).toBeTruthy();
    // A Failed status followed by a node update that drops it keeps it listed too.
    act(() => feed({ type: 'agent_status', agent: 'ghost-2', node: 'n2', status: 'Failed' }, { type: 'node_update', ...node('n2', 'beta') }));
    const list = within(screen.getByRole('list', { name: 'Agents' }));
    expect(list.getByText('ghost-2').closest('button')!.textContent).toContain('on beta');
  });
});
