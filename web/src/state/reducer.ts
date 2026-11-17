// Pure state machine, ported from crates/ulak/src/model.rs (apply + helpers). No I/O, no signals.
import type {
  AgentStatus, Blocker, BlockerKind, HubToUi, NodeId, NodeInfo, Question, UpdateState, VersionInfo,
} from '../proto';
import type { Event, State, TermBuf, Toast, ToastKind } from './types';

/** Per agent; older chunks are dropped first. A reconnect to an agent starts with a full redraw. */
export const TERMINAL_CAP_BYTES = 1 << 20;
export const MAILBOX_LIMIT = 200;
/** An error toast disappears this long after the tick that first saw it. */
export const TOAST_TTL_MS = 8_000;
const MAX_TOASTS = 4;

export function initialState(now = Date.now()): State {
  return {
    conn: 'connecting', nodes: [], questions: [], blockers: [], statuses: {}, mailbox: [], devices: [],
    otp: null, terminals: {}, selected: null, toasts: [], nextToastId: 1, now,
    hubVersion: null, nodeVersions: {}, updates: {},
  };
}

/** Insert or replace by `same`, keeping position. */
export function upsert<T>(list: readonly T[], item: T, same: (a: T, b: T) => boolean): T[] {
  const i = list.findIndex((x) => same(x, item));
  if (i < 0) return [...list, item];
  const out = list.slice();
  out[i] = item;
  return out;
}

const withoutKeys = <V>(rec: Record<string, V>, keys: Iterable<string>): Record<string, V> => {
  const out = { ...rec };
  for (const k of keys) delete out[k];
  return out;
};

/** Updates and versions of an Offline node are stale: forget them. */
function forgetOffline(s: State): State {
  const off = s.nodes.filter((n) => n.state === 'Offline').map((n) => n.id);
  if (!off.some((id) => id in s.updates || id in s.nodeVersions)) return s;
  return { ...s, updates: withoutKeys(s.updates, off), nodeVersions: withoutKeys(s.nodeVersions, off) };
}

/** A tagged frame without its `type` tag. */
const body = <T extends { type: string }>(f: T): Omit<T, 'type'> => {
  const { type: _t, ...rest } = f;
  return rest;
};

const byTime = (a: Blocker, b: Blocker) => a.at_ms - b.at_ms;
const addBlocker = (list: readonly Blocker[], b: Blocker) => upsert(list, b, (x, y) => x.id === y.id).sort(byTime);

export function appendPane(buf: TermBuf | undefined, data: Uint8Array, cap = TERMINAL_CAP_BYTES): TermBuf {
  if (data.length === 0 && buf) return buf;
  const piece = data.length > cap ? data.slice(data.length - cap) : data;
  let chunks = [...(buf?.chunks ?? []), piece];
  let bytes = (buf?.bytes ?? 0) + piece.length;
  while (bytes > cap && chunks.length > 1) {
    bytes -= chunks[0]!.length;
    chunks = chunks.slice(1);
  }
  return { chunks, bytes, seq: (buf?.seq ?? 0) + 1 };
}

/** All buffered bytes of a terminal as one array (for a terminal emulator). */
export function termBytes(buf: TermBuf | undefined): Uint8Array {
  const out = new Uint8Array(buf?.bytes ?? 0);
  let at = 0;
  for (const c of buf?.chunks ?? []) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

export function addToast(s: State, kind: ToastKind, text: string): State {
  const last = s.toasts[s.toasts.length - 1];
  if (last && last.kind === kind && last.text === text) return s;
  const toast: Toast = { id: s.nextToastId, kind, text, expires: null };
  return { ...s, nextToastId: s.nextToastId + 1, toasts: [...s.toasts, toast].slice(-MAX_TOASTS) };
}

/** Stamp the expiry on the first tick that sees a toast, drop the ones past it. */
function ageToasts(s: State): State {
  if (s.toasts.length === 0) return s;
  const toasts = s.toasts
    .map((t) => (t.expires === null ? { ...t, expires: s.now + TOAST_TTL_MS } : t))
    .filter((t) => (t.expires ?? 0) > s.now);
  return { ...s, toasts };
}

export function reduce(s: State, e: Event): State {
  switch (e.kind) {
    case 'conn': {
      if (e.state === s.conn) return s;
      const next = { ...s, conn: e.state };
      // Connected clears old error toasts (the Rust UI cleared last_error).
      return e.state === 'open' ? { ...next, toasts: s.toasts.filter((t) => t.kind !== 'error') } : next;
    }
    case 'tick':
      return ageToasts({ ...s, now: e.now });
    case 'toast':
      return addToast(s, e.toast, e.text);
    case 'dismiss_toast':
      return { ...s, toasts: s.toasts.filter((t) => t.id !== e.id) };
    case 'hub':
      return reduceHub(s, e.frame);
  }
}

function reduceHub(s: State, f: HubToUi): State {
  switch (f.type) {
    case 'snapshot':
      return forgetOffline({ ...s, nodes: f.nodes, questions: f.questions });
    case 'node_update': {
      const n = body(f);
      return forgetOffline({ ...s, nodes: upsert(s.nodes, n, (a, b) => a.id === b.id) });
    }
    case 'question_opened': {
      const q = body(f);
      return { ...s, questions: upsert(s.questions, q, (a, b) => a.id === b.id) };
    }
    case 'question_closed':
      return { ...s, questions: s.questions.filter((q) => q.id !== f.id) };
    case 'message_logged': {
      const m = body(f);
      return { ...s, mailbox: upsert(s.mailbox, m, (a, b) => a.id === b.id) };
    }
    case 'mailbox_log':
      return { ...s, mailbox: f.messages };
    case 'pane_output':
      return { ...s, terminals: { ...s.terminals, [f.agent]: appendPane(s.terminals[f.agent], f.data) } };
    case 'agent_status': {
      const nodes = s.nodes.map((n) =>
        n.id === f.node && !n.agents.includes(f.agent) ? { ...n, agents: [...n.agents, f.agent] } : n,
      );
      return { ...s, nodes, statuses: { ...s.statuses, [f.agent]: f.status } };
    }
    case 'otp_offer': {
      const o = body(f);
      return { ...s, otp: o };
    }
    case 'devices':
      return { ...s, devices: f.devices };
    case 'blockers':
      return { ...s, blockers: f.blockers.reduce(addBlocker, [] as Blocker[]) };
    case 'blocker_opened': {
      const b = body(f);
      return { ...s, blockers: addBlocker(s.blockers, b) };
    }
    case 'blocker_closed':
      return { ...s, blockers: s.blockers.filter((b) => b.id !== f.id) };
    case 'hub_version': {
      const v = body(f);
      return { ...s, hubVersion: v };
    }
    case 'node_version':
      return { ...s, nodeVersions: { ...s.nodeVersions, [f.node]: f.info } };
    case 'update_status':
      return { ...s, updates: { ...s.updates, [f.node]: f.status } };
    case 'error':
      return addToast(s, 'error', f.message);
  }
}

// ---- selectors (pure; wrap in `computed` for signals) ----

export const nodeOf = (s: State, agent: string): NodeInfo | undefined => s.nodes.find((n) => n.agents.includes(agent));
export const questionAgeMs = (s: State, q: Question): number => Math.max(0, s.now - q.at_ms);

/** Open blockers of `profile` on `node`, or on any node when `node` is undefined. */
export const blockersFor = (s: State, node: NodeId | undefined, profile: string): Blocker[] =>
  s.blockers.filter((b) => b.profile === profile && (node === undefined || b.node === node));

export const kindTitle = (k: BlockerKind): string => (k === 'Login' ? 'Login required' : 'Install failed');

export type VersionFlag = 'differs from hub' | 'protocol too old';
/** How a version differs from the hub's; an older protocol wins over any other difference. */
export function versionFlag(hub: VersionInfo, v: VersionInfo): VersionFlag | null {
  if (v.proto < hub.proto) return 'protocol too old';
  return v.proto !== hub.proto || v.version !== hub.version || v.git !== hub.git ? 'differs from hub' : null;
}
export const nodeFlag = (s: State, n: NodeId): VersionFlag | null => {
  const v = s.nodeVersions[n];
  return s.hubVersion && v ? versionFlag(s.hubVersion, v) : null;
};

/** The version an update would install, when one is available. */
export const availableVersion = (u: UpdateState | undefined): string | null => (u?.state === 'available' ? u.version : null);
export const updatesAvailable = (s: State): boolean => Object.values(s.updates).some((u) => availableVersion(u) !== null);

/** Coarse bucket for dots/pills: Running is ok, Starting/Installing info, NeedsLogin/Failed need the user. */
export function agentTone(st: AgentStatus | undefined): 'ok' | 'info' | 'warn' | 'bad' | 'muted' {
  if (st === undefined) return 'muted';
  if (typeof st === 'object') return st.Exited === 0 || st.Exited === null ? 'muted' : 'bad';
  switch (st) {
    case 'Running': return 'ok';
    case 'Starting': case 'Installing': return 'info';
    case 'NeedsLogin': return 'warn';
    case 'Failed': return 'bad';
  }
}

export function agentStatusLabel(st: AgentStatus | undefined): string {
  if (st === undefined) return 'Unknown';
  if (typeof st === 'object') return st.Exited === null ? 'Exited' : `Exited (${st.Exited})`;
  return st === 'NeedsLogin' ? 'Needs login' : st;
}
