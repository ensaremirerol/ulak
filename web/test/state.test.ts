import { describe, expect, it } from 'vitest';
import type { Blocker, HubToUi, NodeInfo, Question, VersionInfo, Message } from '../src/proto';
import {
  appendPane, blockersFor, createStore, defaultSpawnForm, initialState, intentToFrames, nodeFlag, nodeOf,
  questionAgeMs, reduce, spawnFormToFrame, termBytes, TERMINAL_CAP_BYTES, TOAST_TTL_MS, updatesAvailable,
  versionFlag, applyIntentLocally, STATS_CAP, emptyProfileForm, profileErrors, IntentError, settingsToForm, settingsErrors,
} from '../src/state';
import type { Intent, ProfileForm, SettingsForm, State } from '../src/state';

const tel = { load1: 0, cpu_pct: 0, gpu_pct: null, mem_used_pct: 0, at_ms: 0 };
const node = (id: string, hostname: string, agents: string[] = []): NodeInfo => ({ id, hostname, state: 'Idle', telemetry: tel, agents });
const question = (id: string, at_ms: number): Question => ({ id, from: 'a1', text: 'ok?', at_ms });
const message = (id: string): Message => ({ id, from: 'a1', to: 'a2', thread: 't', payload: 'p', status: 'Pending', at_ms: 1 });
const blocker = (id: string, n: string, kind: Blocker['kind'], at_ms: number, profile = 'claude'): Blocker =>
  ({ id, node: n, profile, kind, agent: 'a1', detail: 'd', at_ms });
const vi = (version: string, git: string, proto: number): VersionInfo => ({ version, git, proto, os: 'linux', arch: 'x86_64' });
const bytes = (s: string) => new TextEncoder().encode(s);

const hub = (s: State, frame: HubToUi): State => reduce(s, { kind: 'hub', frame });
const apply = (s: State, ...frames: HubToUi[]): State => frames.reduce(hub, s);
const fresh = () => initialState(0);
/** Dispatch an intent the way the store does: frames, then local effects. */
const dispatch = (s: State, i: Intent) => ({ frames: intentToFrames(s, i), state: applyIntentLocally(s, i) });

describe('reducer', () => {
  it('snapshot replaces nodes and questions', () => {
    let s = apply(fresh(), { type: 'snapshot', nodes: [node('n1', 'a')], questions: [question('q1', 1)] });
    s = hub(s, { type: 'snapshot', nodes: [node('n2', 'b')], questions: [] });
    expect(s.nodes.map((n) => n.id)).toEqual(['n2']);
    expect(s.questions).toEqual([]);
  });

  it('node_update upserts by id keeping position', () => {
    const s = apply(fresh(),
      { type: 'node_update', ...node('n1', 'old') },
      { type: 'node_update', ...node('n2', 'x') },
      { type: 'node_update', ...node('n1', 'new', ['a1']) });
    expect(s.nodes.map((n) => n.hostname)).toEqual(['new', 'x']);
    expect(nodeOf(s, 'a1')?.id).toBe('n1');
    expect(s.nodes[0]).not.toHaveProperty('type');
  });

  it('questions open (deduped), age from tick, close', () => {
    let s = apply(fresh(),
      { type: 'question_opened', ...question('q1', 1000) },
      { type: 'question_opened', ...question('q1', 1000) },
      { type: 'question_opened', ...question('q2', 2000) });
    expect(s.questions).toHaveLength(2);
    s = reduce(s, { kind: 'tick', now: 4000 });
    expect(questionAgeMs(s, s.questions[0]!)).toBe(3000);
    s = hub(s, { type: 'question_closed', id: 'q1' });
    expect(s.questions.map((q) => q.id)).toEqual(['q2']);
  });

  it('age never underflows when the clock is behind', () => {
    expect(questionAgeMs(fresh(), question('q', 99))).toBe(0);
  });

  it('mailbox dedupes and the log replaces', () => {
    let s = apply(fresh(), { type: 'message_logged', ...message('m1') }, { type: 'message_logged', ...message('m1') },
      { type: 'message_logged', ...message('m2') });
    expect(s.mailbox).toHaveLength(2);
    s = hub(s, { type: 'mailbox_log', messages: [message('m9')] });
    expect(s.mailbox.map((m) => m.id)).toEqual(['m9']);
  });

  it('agent_status is kept and registers the agent on its node once', () => {
    let s = hub(fresh(), { type: 'node_update', ...node('n1', 'a') });
    s = hub(s, { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' });
    expect(s.statuses['a1']).toBe('Running');
    expect(s.nodes[0]!.agents).toEqual(['a1']);
    s = hub(s, { type: 'agent_status', agent: 'a1', node: 'n1', status: { Exited: 0 } });
    expect(s.statuses['a1']).toEqual({ Exited: 0 });
    expect(s.nodes[0]!.agents).toEqual(['a1']);
  });

  it('agent_statuses applies like agent_status frames', () => {
    let s = hub(fresh(), { type: 'node_update', ...node('n1', 'a') });
    s = hub(s, { type: 'agent_statuses', statuses: [['a1', 'n1', 'Running'], ['a2', 'n1', { Exited: 1 }]] });
    expect(s.statuses).toEqual({ a1: 'Running', a2: { Exited: 1 } });
    expect(s.nodes[0]!.agents).toEqual(['a1', 'a2']);
  });

  it('agent_failed stores the reason and raises an error toast', () => {
    const s = hub(fresh(), { type: 'agent_failed', agent: 'a1', node: 'n1', reason: 'clone failed' });
    expect(s.lastFailure['a1']).toEqual({ node: 'n1', reason: 'clone failed', at: 0 });
    expect(s.toasts.map((t) => [t.kind, t.text])).toEqual([['error', 'agent a1 failed: clone failed']]);
  });

  it('credentials, repos, branches and queue frames are stored', () => {
    const item = { id: 'q1', agent: 'a1', source: { kind: 'folder', path: '/x' }, target: 'Auto', profile: 'claude',
      prompt_preview: '', created_ms: 1, state: 'queued' } as unknown as import('../src/proto').QueuedSpawn;
    const repo = { full_name: 'o/n', private: false, default_branch: 'main', pushed_at_ms: 0 };
    let s = apply(fresh(),
      { type: 'credentials', credentials: [{ id: 'c1', name: 'n', kind: 'github', login: 'me', created_ms: 1 }] },
      { type: 'repos', credential: 'c1', query: null, repos: [repo], has_more: true },
      { type: 'repos', credential: 'c1', query: 'x', repos: [], has_more: false },
      { type: 'branches', credential: 'c1', repo: 'o/n', branches: ['main'], default_branch: 'main' },
      { type: 'queue', items: [item] },
      { type: 'queue_changed', item: { ...item, state: { Failed: 'boom' } } });
    expect(s.credentials[0]!.login).toBe('me');
    expect(s.repos['c1\0']).toEqual({ repos: [repo], hasMore: true });
    expect(s.repos['c1\0x']!.repos).toEqual([]);
    expect(s.branches['c1\0o/n']!.defaultBranch).toBe('main');
    expect(s.queue).toHaveLength(1);
    expect(s.queue[0]!.state).toEqual({ Failed: 'boom' });
    s = hub(s, { type: 'queue_removed', id: 'q1' });
    expect(s.queue).toEqual([]);
  });

  it('pane output goes to the right agent buffer', () => {
    const s = apply(fresh(), { type: 'pane_output', agent: 'a1', data: bytes('one') },
      { type: 'pane_output', agent: 'a2', data: bytes('two') }, { type: 'pane_output', agent: 'a1', data: bytes('!') });
    expect(new TextDecoder().decode(termBytes(s.terminals['a1']))).toBe('one!');
    expect(new TextDecoder().decode(termBytes(s.terminals['a2']))).toBe('two');
    expect(s.terminals['a1']!.seq).toBe(2);
  });

  it('pane buffer is capped, trims oldest chunks, keeps a lone oversized chunk tail', () => {
    let b = appendPane(undefined, bytes('aaaa'), 10);
    b = appendPane(b, bytes('bbbb'), 10);
    b = appendPane(b, bytes('cccc'), 10);
    expect(b.bytes).toBe(8);
    expect(new TextDecoder().decode(termBytes(b))).toBe('bbbbcccc');
    const big = appendPane(undefined, bytes('0123456789ABCDEF'), 10);
    expect(new TextDecoder().decode(termBytes(big))).toBe('6789ABCDEF');
    expect(appendPane(b, new Uint8Array(0), 10)).toBe(b);
    expect(TERMINAL_CAP_BYTES).toBe(1 << 20);
  });

  it('otp offer, devices and error', () => {
    let s = hub(fresh(), { type: 'otp_offer', kind: 'ui', otp: 'ABCD2345', hub_fingerprint: 'AA:BB', expires_ms: 9 });
    expect(s.otp).toEqual({ kind: 'ui', otp: 'ABCD2345', hub_fingerprint: 'AA:BB', expires_ms: 9 });
    const d = { id: 'd1', kind: 'ui' as const, hostname: 'h', created_ms: 1, revoked: false };
    s = hub(s, { type: 'devices', devices: [d, d] });
    expect(s.devices).toHaveLength(2);
    s = hub(s, { type: 'error', message: 'nope' });
    expect(s.toasts.map((t) => [t.kind, t.text])).toEqual([['error', 'nope']]);
    expect(hub(s, { type: 'error', message: 'nope' }).toasts).toHaveLength(1);
  });

  it('conn events set state and open clears error toasts only', () => {
    let s = fresh();
    expect(s.conn).toBe('connecting');
    s = reduce(s, { kind: 'toast', toast: 'error', text: 'old' });
    s = reduce(s, { kind: 'toast', toast: 'info', text: 'keep' });
    s = reduce(s, { kind: 'conn', state: 'open' });
    expect(s.conn).toBe('open');
    expect(s.toasts.map((t) => t.text)).toEqual(['keep']);
    expect(reduce(s, { kind: 'conn', state: 'open' })).toBe(s);
    expect(reduce(s, { kind: 'conn', state: 'reconnecting' }).conn).toBe('reconnecting');
  });

  it('toasts expire a TTL after the first tick that sees them', () => {
    let s = reduce(fresh(), { kind: 'tick', now: 1000 });
    s = reduce(s, { kind: 'toast', toast: 'error', text: 'boom' });
    s = reduce(s, { kind: 'tick', now: 2000 });
    s = reduce(s, { kind: 'tick', now: 2000 + TOAST_TTL_MS - 1 });
    expect(s.toasts).toHaveLength(1);
    s = reduce(s, { kind: 'tick', now: 2000 + TOAST_TTL_MS });
    expect(s.toasts).toEqual([]);
    s = reduce(s, { kind: 'toast', toast: 'error', text: 'again' });
    s = reduce(s, { kind: 'tick', now: 20_000 });
    s = reduce(s, { kind: 'tick', now: 20_001 });
    expect(s.toasts).toHaveLength(1);
  });

  it('toasts dismiss by id and cap at four', () => {
    let s = fresh();
    for (let i = 0; i < 6; i++) s = reduce(s, { kind: 'toast', toast: 'info', text: `t${i}` });
    expect(s.toasts.map((t) => t.text)).toEqual(['t2', 't3', 't4', 't5']);
    s = reduce(s, { kind: 'dismiss_toast', id: s.toasts[0]!.id });
    expect(s.toasts).toHaveLength(3);
  });

  it('blockers dedupe, sort by time, close; blockersFor filters', () => {
    const b1 = blocker('b1', 'n1', 'Login', 20);
    const b2 = blocker('b2', 'n2', 'Install', 10);
    let s = hub(fresh(), { type: 'blockers', blockers: [b1, b2] });
    expect(s.blockers.map((b) => b.id)).toEqual(['b2', 'b1']);
    s = hub(s, { type: 'blocker_opened', ...b1 });
    expect(s.blockers).toHaveLength(2);
    expect(blockersFor(s, 'n1', 'claude')).toHaveLength(1);
    expect(blockersFor(s, undefined, 'claude')).toHaveLength(2);
    expect(blockersFor(s, undefined, 'other')).toHaveLength(0);
    s = hub(s, { type: 'blocker_closed', id: 'b2' });
    expect(s.blockers.map((b) => b.id)).toEqual(['b1']);
  });

  it('version flags', () => {
    const h = vi('0.2.0', 'aaa', 2);
    expect(versionFlag(h, vi('0.2.0', 'aaa', 2))).toBeNull();
    expect(versionFlag(h, vi('0.1.0', 'aaa', 2))).toBe('differs from hub');
    expect(versionFlag(h, vi('0.2.0', 'bbb', 2))).toBe('differs from hub');
    expect(versionFlag(h, vi('0.2.0', 'aaa', 3))).toBe('differs from hub');
    expect(versionFlag(h, vi('0.2.0', 'aaa', 1))).toBe('protocol too old');
    expect(updatesAvailable({ ...fresh(), updates: { n: { state: 'checking' } } })).toBe(false);
    expect(updatesAvailable({ ...fresh(), updates: { n: { state: 'available', version: '1' } } })).toBe(true);
  });

  it('version frames fill state; an Offline node forgets them', () => {
    let s = apply(fresh(), { type: 'node_update', ...node('n1', 'a') }, { type: 'hub_version', ...vi('0.2.0', 'h', 1) },
      { type: 'node_version', node: 'n1', info: vi('0.1.0', 'x', 1) }, { type: 'update_status', node: 'n1', status: { state: 'checking' } });
    expect(s.hubVersion?.git).toBe('h');
    expect(s.hubVersion).not.toHaveProperty('type');
    expect(nodeFlag(s, 'n1')).toBe('differs from hub');
    expect(s.updates['n1']).toEqual({ state: 'checking' });
    s = hub(s, { type: 'node_update', ...node('n1', 'a'), state: 'Offline' });
    expect(s.updates).toEqual({});
    expect(s.nodeVersions).toEqual({});
  });
});

describe('intents', () => {
  it('answer yields a frame and leaves the question; empty answer is an error', () => {
    const s = hub(fresh(), { type: 'question_opened', ...question('q1', 1) });
    const r = dispatch(s, { kind: 'answer', question: 'q1', text: 'yes' });
    expect(r.frames).toEqual([{ type: 'answer', question: 'q1', text: 'yes' }]);
    expect(r.state.questions).toHaveLength(1);
    expect(() => intentToFrames(s, { kind: 'answer', question: 'q1', text: '  ' })).toThrow();
  });

  it('selecting detaches the previous agent first and is idempotent', () => {
    let r = dispatch(fresh(), { kind: 'select', agent: 'a1' });
    expect(r.frames).toEqual([{ type: 'attach', agent: 'a1' }]);
    r = dispatch(r.state, { kind: 'select', agent: 'a2' });
    expect(r.frames).toEqual([{ type: 'detach', agent: 'a1' }, { type: 'attach', agent: 'a2' }]);
    expect(dispatch(r.state, { kind: 'select', agent: 'a2' }).frames).toEqual([]);
    r = dispatch(r.state, { kind: 'detach' });
    expect(r.frames).toEqual([{ type: 'detach', agent: 'a2' }]);
    expect(r.state.selected).toBeNull();
    expect(dispatch(r.state, { kind: 'detach' }).frames).toEqual([]);
  });

  it('select clears the selected agent buffer (full redraw follows)', () => {
    let s = hub(fresh(), { type: 'pane_output', agent: 'a1', data: bytes('stale') });
    s = hub(s, { type: 'pane_output', agent: 'a2', data: bytes('other') });
    s = dispatch(s, { kind: 'select', agent: 'a1' }).state;
    expect(s.terminals['a1']).toBeUndefined();
    expect(s.terminals['a2']).toBeDefined();
  });

  it('send_keys targets the selected agent only', () => {
    const s0 = fresh();
    expect(intentToFrames(s0, { kind: 'send_keys', data: bytes('x') })).toEqual([]);
    const s = dispatch(s0, { kind: 'select', agent: 'a1' }).state;
    expect(intentToFrames(s, { kind: 'send_keys', data: bytes('ls\r') })).toEqual([{ type: 'pane_input', agent: 'a1', data: bytes('ls\r') }]);
    expect(intentToFrames(s, { kind: 'send_keys', data: new Uint8Array(0) })).toEqual([]);
  });

  it('spawn form builds a frame or throws', () => {
    const form = { ...defaultSpawnForm(), agentId: 'worker-1', workdir: '/tmp', prompt: 'do it', args: '--a  --b' };
    const f = spawnFormToFrame(form);
    expect(f).toMatchObject({ type: 'spawn', target: 'Auto', spec: { agent_id: 'worker-1', profile: { binary: 'claude', args: ['--a', '--b'], config_dir: null } } });
    expect(spawnFormToFrame({ ...form, node: 'n1' })).toMatchObject({ target: { Node: 'n1' } });
    expect(() => spawnFormToFrame({ ...form, agentId: 'Bad Id' })).toThrow(/Agent name/);
    expect(() => spawnFormToFrame({ ...form, node: 'Bad' })).toThrow(/Node/);
    expect(() => spawnFormToFrame({ ...form, binary: ' ' })).toThrow();
    expect(() => spawnFormToFrame({ ...form, prompt: '' })).toThrow();
  });

  it('config_dir goes into the profile, trimmed', () => {
    const f = spawnFormToFrame({ ...defaultSpawnForm(), agentId: 'a1', workdir: '/w', prompt: 'p', configDir: ' /home/me/.claude-p ' });
    expect(f).toMatchObject({ spec: { profile: { config_dir: '/home/me/.claude-p' } } });
  });

  it('simple intents map to one frame', () => {
    const s = fresh();
    expect(intentToFrames(s, { kind: 'kill', agent: 'a1' })).toEqual([{ type: 'kill', agent: 'a1' }]);
    expect(intentToFrames(s, { kind: 'add_device', device: 'browser' })).toEqual([{ type: 'add_device', kind: 'browser' }]);
    expect(intentToFrames(s, { kind: 'revoke_device', id: 'd1' })).toEqual([{ type: 'revoke_device', id: 'd1' }]);
    expect(intentToFrames(s, { kind: 'refresh_devices' })).toEqual([{ type: 'list_devices' }]);
    expect(intentToFrames(s, { kind: 'refresh_mailbox' })).toEqual([{ type: 'mailbox_log', thread: null, limit: 200 }]);
    expect(intentToFrames(s, { kind: 'resolve_blocker', id: 'b' })).toEqual([{ type: 'resolve_blocker', id: 'b' }]);
  });

  it('update intents map to frames', () => {
    const s = fresh();
    expect(intentToFrames(s, { kind: 'check_update', node: 'n1' })).toEqual([{ type: 'check_update', node: 'n1' }]);
    expect(intentToFrames(s, { kind: 'update_node' })).toEqual([{ type: 'update_node', node: null }]);
  });

  it('local effects of kill and add_device', () => {
    let s = dispatch(fresh(), { kind: 'select', agent: 'a1' }).state;
    expect(dispatch(s, { kind: 'kill', agent: 'a2' }).state.selected).toBe('a1');
    expect(dispatch(s, { kind: 'kill', agent: 'a1' }).state.selected).toBeNull();
    s = hub(s, { type: 'otp_offer', kind: 'ui', otp: 'ABCD2345', hub_fingerprint: '', expires_ms: 1 });
    expect(dispatch(s, { kind: 'add_device', device: 'ui' }).state.otp).toBeNull();
  });
});

describe('store', () => {
  const mk = (result: 'sent' | 'queued' | 'dropped' = 'sent') => {
    const store = createStore(fresh());
    const sent: unknown[] = [];
    store.setSender((f) => { sent.push(f); return result; });
    return { store, sent };
  };

  it('sends frames, applies local effects, resync re-attaches the selection', () => {
    const { store, sent } = mk();
    expect(store.resync()).toEqual([{ type: 'list_credentials' }, { type: 'list_queue' }]);
    expect(store.intent({ kind: 'select', agent: 'a1' })).toBe(true);
    expect(sent).toEqual([{ type: 'attach', agent: 'a1' }]);
    expect(store.state.value.selected).toBe('a1');
    expect(store.resync()).toEqual([{ type: 'list_credentials' }, { type: 'list_queue' }, { type: 'attach', agent: 'a1' }]);
  });

  it('select of a starting agent holds the attach back until it is Running', () => {
    const { store, sent } = mk();
    store.dispatch({ kind: 'hub', frame: { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Starting' } });
    expect(store.intent({ kind: 'select', agent: 'a1' })).toBe(true);
    expect(sent).toEqual([]);
    expect(store.state.value.selected).toBe('a1');
    store.dispatch({ kind: 'hub', frame: { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Installing' } });
    expect(sent).toEqual([]);
    store.dispatch({ kind: 'hub', frame: { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' } });
    expect(sent).toEqual([{ type: 'attach', agent: 'a1' }]);
    store.dispatch({ kind: 'hub', frame: { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' } });
    expect(sent).toHaveLength(1); // only the transition attaches
  });

  it('a starting agent that fails never attaches', () => {
    const { store, sent } = mk();
    store.dispatch({ kind: 'hub', frame: { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Starting' } });
    store.intent({ kind: 'select', agent: 'a1' });
    store.dispatch({ kind: 'hub', frame: { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Failed' } });
    expect(sent).toEqual([]);
  });

  it('a validation error becomes an error toast and sends nothing', () => {
    const { store, sent } = mk();
    expect(store.intent({ kind: 'answer', question: 'q', text: '' })).toBe(false);
    expect(sent).toEqual([]);
    expect(store.state.value.toasts[0]?.kind).toBe('error');
  });

  it('a dropped send applies nothing locally and toasts', () => {
    const { store } = mk('dropped');
    expect(store.intent({ kind: 'select', agent: 'a1' })).toBe(false);
    expect(store.state.value.selected).toBeNull();
    expect(store.state.value.toasts).toHaveLength(1);
  });

  it('onPane delivers chunks of that agent until unsubscribed', () => {
    const { store } = mk();
    const got: string[] = [];
    const off = store.onPane('a1', (d) => got.push(new TextDecoder().decode(d)));
    store.onFrame({ type: 'pane_output', agent: 'a2', data: bytes('no') });
    store.onFrame({ type: 'pane_output', agent: 'a1', data: bytes('yes') });
    off();
    store.onFrame({ type: 'pane_output', agent: 'a1', data: bytes('later') });
    expect(got).toEqual(['yes']);
  });

  it('select derives a deduplicated slice', () => {
    const { store } = mk();
    const n = store.select((s) => s.nodes.length);
    store.onFrame({ type: 'node_update', ...node('n1', 'a') });
    expect(n.value).toBe(1);
  });
});

describe('versionFlag git prefix', () => {
  it('treats a 7 char sha and its 12 char form as the same build', () => {
    const v = { version: '0.1.4', git: 'db27234', proto: 1, os: 'linux', arch: 'x86_64' };
    expect(versionFlag({ ...v, git: 'db27234a1b2c' }, v)).toBeNull();
    expect(versionFlag({ ...v, git: 'ffffffff' }, v)).toBe('differs from hub');
  });
});

describe('node profiles, repos and clean reports', () => {
  it('stores the latest of each per node', () => {
    const p = { name: 'claude', binary: 'claude', config_dir: null, installed: true, login_blocked: false };
    const r = { full_name: 'o/r', path: '/w/r', branch: 'main', clean: true, ahead: 0, behind: null, last_fetch_ms: null, in_use: false, managed: true };
    const item = { path: '/w/r', full_name: 'o/r', reason: 'clean' };
    const s = apply(fresh(),
      { type: 'node_profiles', node: 'n1', profiles: [p] },
      { type: 'node_profiles', node: 'n2', profiles: [] },
      { type: 'node_repos', node: 'n1', repos: [r] },
      { type: 'clean_report', node: 'n1', dry_run: true, removed: [item], kept: [] },
      { type: 'clean_report', node: 'n1', dry_run: false, removed: [], kept: [item] });
    expect(s.nodeProfiles).toEqual({ n1: [p], n2: [] });
    expect(s.nodeRepos).toEqual({ n1: [r] });
    expect(s.lastClean).toEqual({ n1: { dryRun: false, removed: [], kept: [item] } });
  });
});

describe('hub config, node stats (0.1.7)', () => {
  const t = (at_ms: number, cpu_pct = 1) => ({ ...tel, at_ms, cpu_pct });
  const watch = (s: State, n: string) => dispatch(s, { kind: 'watch_node', node: n });

  it('hub_config replaces profiles and settings', () => {
    const p = { node: null, profile: { name: 'a', binary: 'claude' } };
    let s = apply(fresh(), { type: 'hub_config', profiles: [p], settings: [{ node: 'n1', settings: { clean_auto: true } }] });
    expect(s.hubProfiles).toEqual([p]);
    expect(s.nodeSettings).toEqual({ n1: { clean_auto: true } });
    s = apply(s, { type: 'hub_config', profiles: [], settings: [] });
    expect(s.hubProfiles).toEqual([]);
    expect(s.nodeSettings).toEqual({});
  });

  it('watch_node asks for the history; the reply sets it (capped) only for the watched node', () => {
    const w = watch(fresh(), 'n1');
    expect(w.frames).toEqual([{ type: 'node_stats', node: 'n1' }]);
    expect(w.state.stats).toEqual({ node: 'n1', connectedMs: null, samples: [] });
    const many = Array.from({ length: STATS_CAP + 5 }, (_, i) => t(i + 1));
    let s = apply(w.state, { type: 'node_stats', node: 'n2', connected_ms: 5, samples: [t(1)] });
    expect(s.stats!.samples).toEqual([]);
    s = apply(s, { type: 'node_stats', node: 'n1', connected_ms: 5, samples: many });
    expect(s.stats!.connectedMs).toBe(5);
    expect(s.stats!.samples).toHaveLength(STATS_CAP);
    expect(s.stats!.samples[0]!.at_ms).toBe(6);
    expect(dispatch(s, { kind: 'unwatch_node', node: 'n2' }).state).toBe(s);
    expect(dispatch(s, { kind: 'unwatch_node', node: 'n1' })).toEqual({ frames: [], state: { ...s, stats: null } });
  });

  it('node_update appends new telemetry of the watched node only, capped at 720', () => {
    let s = watch(apply(fresh(), { type: 'snapshot', nodes: [node('n1', 'a'), node('n2', 'b')], questions: [] }), 'n1').state;
    s = apply(s, { type: 'node_stats', node: 'n1', connected_ms: null, samples: Array.from({ length: STATS_CAP }, (_, i) => t(i + 1)) });
    const upd = (id: string, at: number, state: NodeInfo['state'] = 'Idle'): HubToUi => ({ type: 'node_update', ...node(id, id), state, telemetry: t(at, 50) });
    s = apply(s, upd('n1', STATS_CAP + 1), upd('n1', STATS_CAP + 1), upd('n2', 9_999), upd('n1', 99_999, 'Offline'));
    expect(s.stats!.samples).toHaveLength(STATS_CAP);
    expect(s.stats!.samples[0]!.at_ms).toBe(2);
    expect(s.stats!.samples.at(-1)).toEqual(t(STATS_CAP + 1, 50));
  });

  it('resync asks for the watched node history again', () => {
    const store = createStore(watch(fresh(), 'n1').state);
    expect(store.resync()).toContainEqual({ type: 'node_stats', node: 'n1' });
  });
});

describe('profile and settings intents', () => {
  const form = (over: Partial<ProfileForm> = {}): ProfileForm => ({ ...emptyProfileForm(), name: 'claude-work', binary: ' claude ', ...over });
  const save = (f: ProfileForm, node: string | null = null, keepEnv?: Record<string, string>) => intentToFrames(fresh(), { kind: 'save_profile', node, form: f, keepEnv });

  it('save_profile builds the wire profile, leaving empty fields out', () => {
    expect(save(form(), 'n1')).toEqual([{ type: 'save_profile', node: 'n1', profile: { name: 'claude-work', binary: 'claude' } }]);
    expect(save(form({ args: ' --model  opus ', configDir: ' ~/.cw ', env: 'HTTPS_PROXY=http://p:3128\n\nA=x=y' }))).toEqual([{
      type: 'save_profile', node: null,
      profile: { name: 'claude-work', binary: 'claude', args: ['--model', 'opus'], config_dir: '~/.cw', env: { HTTPS_PROXY: 'http://p:3128', A: 'x=y' } },
    }]);
  });

  it('kept env survives an edit; typed lines override it', () => {
    expect(save(form({ env: 'B=new' }), null, { A: 'secret', B: 'old' })[0]).toMatchObject({ profile: { env: { A: 'secret', B: 'new' } } });
  });

  it('validation mirrors dto.rs', () => {
    expect(profileErrors(form({ name: 'bad name' })).name).toBeTruthy();
    expect(profileErrors(form({ name: 'x'.repeat(65) })).name).toBeTruthy();
    expect(profileErrors(form({ binary: ' ' })).binary).toBeTruthy();
    expect(profileErrors(form({ args: Array(33).fill('a').join(' ') })).args).toBeTruthy();
    expect(profileErrors(form({ env: 'ULAK_TOKEN=x' })).env).toMatch(/reserved/);
    expect(profileErrors(form({ env: '1X=x' })).env).toMatch(/not a valid name/);
    expect(profileErrors(form({ env: 'NOVALUE' })).env).toMatch(/KEY=value/);
    expect(profileErrors(form(), Object.fromEntries(Array.from({ length: 33 }, (_, i) => [`K${i}`, 'v']))).env).toMatch(/32/);
    expect(profileErrors(form())).toEqual({});
    expect(() => save(form({ env: 'ulak_x=1' }))).toThrow(IntentError);
  });

  it('delete_profile', () => {
    expect(intentToFrames(fresh(), { kind: 'delete_profile', node: 'n1', name: 'p' })).toEqual([{ type: 'delete_profile', node: 'n1', name: 'p' }]);
  });

  it('save_node_settings: empty and inherit become absent, tri-state maps to bool', () => {
    const f: SettingsForm = { ...settingsToForm(), repoRoots: ' ~/code \n\n/srv ', indexIntervalMins: '30', cleanAuto: 'on', autoUpdate: 'off', cpuBusyPct: '85.5' };
    expect(intentToFrames(fresh(), { kind: 'save_node_settings', node: 'n1', form: f })).toEqual([{
      type: 'save_node_settings', node: 'n1',
      settings: { repo_roots: ['~/code', '/srv'], index_interval_mins: 30, clean_auto: true, auto_update: false, cpu_busy_pct: 85.5 },
    }]);
    expect(intentToFrames(fresh(), { kind: 'save_node_settings', node: 'n1', form: settingsToForm() })).toEqual([{ type: 'save_node_settings', node: 'n1', settings: {} }]);
    expect(settingsToForm({ clean_auto: false, gpu_busy_pct: 50 })).toMatchObject({ cleanAuto: 'off', autoUpdate: 'inherit', gpuBusyPct: '50' });
  });

  it('settings ranges', () => {
    const bad = (over: Partial<SettingsForm>) => Object.keys(settingsErrors({ ...settingsToForm(), ...over }));
    expect(bad({ indexIntervalMins: '0' })).toEqual(['indexIntervalMins']);
    expect(bad({ indexIntervalMins: '1441' })).toEqual(['indexIntervalMins']);
    expect(bad({ indexIntervalMins: '1.5' })).toEqual(['indexIntervalMins']);
    expect(bad({ cleanAfterExitHours: '8761' })).toEqual(['cleanAfterExitHours']);
    expect(bad({ cpuBusyPct: '101' })).toEqual(['cpuBusyPct']);
    expect(bad({ gpuBusyPct: 'x' })).toEqual(['gpuBusyPct']);
    expect(bad({ repoRoots: Array.from({ length: 17 }, (_, i) => `/r${i}`).join('\n') })).toEqual(['repoRoots']);
    expect(bad({ indexIntervalMins: '1440', cleanAfterExitHours: '1', cpuBusyPct: '99.5', gpuBusyPct: '1' })).toEqual([]);
    expect(() => intentToFrames(fresh(), { kind: 'save_node_settings', node: 'n1', form: { ...settingsToForm(), cpuBusyPct: '0' } })).toThrow(IntentError);
  });
});
