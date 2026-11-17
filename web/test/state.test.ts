import { describe, expect, it } from 'vitest';
import type { Blocker, HubToUi, NodeInfo, Question, VersionInfo, Message } from '../src/proto';
import {
  appendPane, blockersFor, createStore, defaultSpawnForm, initialState, intentToFrames, nodeFlag, nodeOf,
  questionAgeMs, reduce, spawnFormToFrame, termBytes, TERMINAL_CAP_BYTES, TOAST_TTL_MS, updatesAvailable,
  versionFlag, applyIntentLocally,
} from '../src/state';
import type { Intent, State } from '../src/state';

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
    expect(store.resync()).toEqual([]);
    expect(store.intent({ kind: 'select', agent: 'a1' })).toBe(true);
    expect(sent).toEqual([{ type: 'attach', agent: 'a1' }]);
    expect(store.state.value.selected).toBe('a1');
    expect(store.resync()).toEqual([{ type: 'attach', agent: 'a1' }]);
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
