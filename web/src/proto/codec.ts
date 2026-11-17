import { b64ToBytes, bytesToB64 } from './bytes';
import type {
  AgentStatus, Blocker, DeviceInfo, HubToUi, HubToUiType, Message, NodeInfo, NodeState,
  OtpOffer, Question, SpawnSpec, Telemetry, UiToHub, UiToHubType, UnknownFrame, UpdateState,
  VersionInfo,
} from './types';

class Bad extends Error {}
type Obj = Record<string, unknown>;

const fail = (what: string): never => {
  throw new Bad(what);
};
const obj = (v: unknown, what: string): Obj =>
  typeof v === 'object' && v !== null && !Array.isArray(v) ? (v as Obj) : fail(`${what}: not an object`);
const str = (v: unknown, what: string): string => (typeof v === 'string' ? v : fail(`${what}: not a string`));
const num = (v: unknown, what: string): number =>
  typeof v === 'number' && Number.isFinite(v) ? v : fail(`${what}: not a number`);
const bool = (v: unknown, what: string): boolean => (typeof v === 'boolean' ? v : fail(`${what}: not a bool`));
const arr = <T>(v: unknown, what: string, f: (x: unknown) => T): T[] =>
  Array.isArray(v) ? v.map(f) : fail(`${what}: not an array`);
const opt = <T>(v: unknown, f: (x: unknown) => T): T | null => (v === null || v === undefined ? null : f(v));
const oneOf = <T extends string>(v: unknown, set: readonly T[], what: string): T =>
  typeof v === 'string' && (set as readonly string[]).includes(v) ? (v as T) : fail(`${what}: unexpected ${String(v)}`);

const DEVICE_KINDS = ['node', 'ui', 'browser'] as const;
const MESSAGE_STATUS = ['Pending', 'Delivered', 'Read'] as const;
const UPDATE_PLAIN = ['checking', 'downloading', 'verifying', 'installing', 'restarting'] as const;

function telemetry(v: unknown): Telemetry {
  const o = obj(v, 'telemetry');
  return {
    load1: num(o.load1, 'load1'),
    cpu_pct: num(o.cpu_pct, 'cpu_pct'),
    gpu_pct: opt(o.gpu_pct, (x) => num(x, 'gpu_pct')),
    mem_used_pct: num(o.mem_used_pct, 'mem_used_pct'),
    at_ms: num(o.at_ms, 'at_ms'),
  };
}

function nodeState(v: unknown): NodeState {
  if (v === 'Idle' || v === 'Offline') return v;
  const busy = obj(obj(v, 'node state'), 'busy').Busy;
  if (busy === 'Load') return { Busy: 'Load' };
  const g = obj(obj(busy, 'busy reason').Gaming, 'gaming');
  return { Busy: { Gaming: { process: str(g.process, 'process') } } };
}

export function agentStatus(v: unknown): AgentStatus {
  if (typeof v === 'string') return oneOf(v, ['Starting', 'Running', 'Failed', 'Installing', 'NeedsLogin'] as const, 'agent status');
  return { Exited: opt(obj(v, 'agent status').Exited, (x) => num(x, 'exit code')) };
}

export function nodeInfo(v: unknown): NodeInfo {
  const o = obj(v, 'node');
  return {
    id: str(o.id, 'id'),
    hostname: str(o.hostname, 'hostname'),
    state: nodeState(o.state),
    telemetry: telemetry(o.telemetry),
    agents: arr(o.agents, 'agents', (x) => str(x, 'agent')),
  };
}

export function question(v: unknown): Question {
  const o = obj(v, 'question');
  return { id: str(o.id, 'id'), from: str(o.from, 'from'), text: str(o.text, 'text'), at_ms: num(o.at_ms, 'at_ms') };
}

export function message(v: unknown): Message {
  const o = obj(v, 'message');
  return {
    id: str(o.id, 'id'),
    from: str(o.from, 'from'),
    to: str(o.to, 'to'),
    thread: str(o.thread, 'thread'),
    payload: str(o.payload, 'payload'),
    status: oneOf(o.status, MESSAGE_STATUS, 'status'),
    at_ms: num(o.at_ms, 'at_ms'),
  };
}

export function blocker(v: unknown): Blocker {
  const o = obj(v, 'blocker');
  return {
    id: str(o.id, 'id'),
    node: str(o.node, 'node'),
    profile: str(o.profile, 'profile'),
    kind: oneOf(o.kind, ['Login', 'Install'] as const, 'kind'),
    agent: opt(o.agent, (x) => str(x, 'agent')),
    detail: str(o.detail, 'detail'),
    at_ms: num(o.at_ms, 'at_ms'),
  };
}

export function deviceInfo(v: unknown): DeviceInfo {
  const o = obj(v, 'device');
  return {
    id: str(o.id, 'id'),
    kind: oneOf(o.kind, DEVICE_KINDS, 'kind'),
    hostname: str(o.hostname, 'hostname'),
    created_ms: num(o.created_ms, 'created_ms'),
    revoked: bool(o.revoked, 'revoked'),
  };
}

export function versionInfo(v: unknown): VersionInfo {
  const o = obj(v, 'version');
  return {
    version: str(o.version, 'version'),
    git: str(o.git, 'git'),
    proto: num(o.proto, 'proto'),
    os: str(o.os, 'os'),
    arch: str(o.arch, 'arch'),
  };
}

export function updateState(v: unknown): UpdateState {
  const o = obj(v, 'update state');
  const s = str(o.state, 'state');
  switch (s) {
    case 'up_to_date': return { state: s, checked_ms: num(o.checked_ms, 'checked_ms') };
    case 'available': return { state: s, version: str(o.version, 'version') };
    case 'failed': return { state: s, message: str(o.message, 'message') };
    default: return { state: oneOf(s, UPDATE_PLAIN, 'update state') };
  }
}

function otpOffer(o: Obj): OtpOffer {
  return {
    kind: oneOf(o.kind, DEVICE_KINDS, 'kind'),
    otp: str(o.otp, 'otp'),
    hub_fingerprint: str(o.hub_fingerprint, 'hub_fingerprint'),
    expires_ms: num(o.expires_ms, 'expires_ms'),
  };
}

const s = (v: unknown, w: string) => str(v, w);

type Decoder<T> = (o: Obj) => T;
// A table, not a switch: `satisfies` makes a missing variant a compile error.
const hubDecoders = {
  snapshot: (o) => ({ type: 'snapshot', nodes: arr(o.nodes, 'nodes', nodeInfo), questions: arr(o.questions, 'questions', question) }),
  blockers: (o) => ({ type: 'blockers', blockers: arr(o.blockers, 'blockers', blocker) }),
  blocker_opened: (o) => ({ type: 'blocker_opened', ...blocker(o) }),
  blocker_closed: (o) => ({ type: 'blocker_closed', id: s(o.id, 'id') }),
  node_update: (o) => ({ type: 'node_update', ...nodeInfo(o) }),
  question_opened: (o) => ({ type: 'question_opened', ...question(o) }),
  question_closed: (o) => ({ type: 'question_closed', id: s(o.id, 'id') }),
  message_logged: (o) => ({ type: 'message_logged', ...message(o) }),
  pane_output: (o) => ({ type: 'pane_output', agent: s(o.agent, 'agent'), data: b64ToBytes(s(o.data, 'data')) }),
  agent_status: (o) => ({ type: 'agent_status', agent: s(o.agent, 'agent'), node: s(o.node, 'node'), status: agentStatus(o.status) }),
  otp_offer: (o) => ({ type: 'otp_offer', ...otpOffer(o) }),
  devices: (o) => ({ type: 'devices', devices: arr(o.devices, 'devices', deviceInfo) }),
  mailbox_log: (o) => ({ type: 'mailbox_log', messages: arr(o.messages, 'messages', message) }),
  hub_version: (o) => ({ type: 'hub_version', ...versionInfo(o) }),
  node_version: (o) => ({ type: 'node_version', node: s(o.node, 'node'), info: versionInfo(o.info) }),
  update_status: (o) => ({ type: 'update_status', node: s(o.node, 'node'), status: updateState(o.status) }),
  error: (o) => ({ type: 'error', message: s(o.message, 'message') }),
} satisfies { [K in HubToUiType]: Decoder<Extract<HubToUi, { type: K }>> };

export const HUB_TO_UI_TYPES = Object.keys(hubDecoders) as HubToUiType[];

/** Parse one text frame from the hub. Never throws: junk and future variants become `unknown`. */
export function decodeHubToUi(text: string): HubToUi | UnknownFrame {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return unknown(text, 'not JSON');
  }
  return decodeHubValue(raw);
}

export function decodeHubValue(raw: unknown): HubToUi | UnknownFrame {
  try {
    const o = obj(raw, 'frame');
    const type = str(o.type, 'type');
    if (!Object.hasOwn(hubDecoders, type)) return unknown(raw, `unknown frame type ${type}`);
    return (hubDecoders[type as HubToUiType] as Decoder<HubToUi>)(o);
  } catch (e) {
    return unknown(raw, e instanceof Bad ? e.message : String(e));
  }
}

function unknown(raw: unknown, reason: string): UnknownFrame {
  console.warn('ulak: ignoring frame:', reason);
  return { type: 'unknown', raw, reason };
}

// ---- UiToHub ----

/** The JSON value of a UiToHub frame (pane bytes as base64). */
export function uiToHubValue(f: UiToHub): Obj {
  return f.type === 'pane_input' ? { type: f.type, agent: f.agent, data: bytesToB64(f.data) } : { ...f };
}

export function encodeUiToHub(f: UiToHub): string {
  return JSON.stringify(uiToHubValue(f));
}

function spawnSpec(v: unknown): SpawnSpec {
  const o = obj(v, 'spec');
  const p = obj(o.profile, 'profile');
  const env = obj(o.env, 'env');
  return {
    agent_id: str(o.agent_id, 'agent_id'),
    profile: {
      binary: str(p.binary, 'binary'),
      args: arr(p.args, 'args', (x) => str(x, 'arg')),
      config_dir: opt(p.config_dir, (x) => str(x, 'config_dir')),
    },
    workdir: str(o.workdir, 'workdir'),
    prompt: str(o.prompt, 'prompt'),
    env: Object.fromEntries(Object.entries(env).map(([k, x]) => [k, str(x, 'env value')])),
  };
}

const optStr = (v: unknown, w: string) => opt(v, (x) => str(x, w));

const uiDecoders = {
  spawn: (o) => ({
    type: 'spawn',
    target: o.target === 'Auto' ? 'Auto' : { Node: str(obj(o.target, 'target').Node, 'node') },
    spec: spawnSpec(o.spec),
  }),
  answer: (o) => ({ type: 'answer', question: s(o.question, 'question'), text: s(o.text, 'text') }),
  attach: (o) => ({ type: 'attach', agent: s(o.agent, 'agent') }),
  detach: (o) => ({ type: 'detach', agent: s(o.agent, 'agent') }),
  pane_input: (o) => ({ type: 'pane_input', agent: s(o.agent, 'agent'), data: b64ToBytes(s(o.data, 'data')) }),
  kill: (o) => ({ type: 'kill', agent: s(o.agent, 'agent') }),
  add_device: (o) => ({ type: 'add_device', kind: oneOf(o.kind, DEVICE_KINDS, 'kind') }),
  revoke_device: (o) => ({ type: 'revoke_device', id: s(o.id, 'id') }),
  list_devices: () => ({ type: 'list_devices' }),
  resolve_blocker: (o) => ({ type: 'resolve_blocker', id: s(o.id, 'id') }),
  check_update: (o) => ({ type: 'check_update', node: optStr(o.node, 'node') }),
  update_node: (o) => ({ type: 'update_node', node: optStr(o.node, 'node') }),
  mailbox_log: (o) => ({ type: 'mailbox_log', thread: optStr(o.thread, 'thread'), limit: num(o.limit, 'limit') }),
} satisfies { [K in UiToHubType]: Decoder<Extract<UiToHub, { type: K }>> };

export const UI_TO_HUB_TYPES = Object.keys(uiDecoders) as UiToHubType[];

/** Only the contract test needs this (the hub is the real consumer); throws on junk. */
export function decodeUiToHubValue(raw: unknown): UiToHub {
  const o = obj(raw, 'frame');
  const type = oneOf(o.type, UI_TO_HUB_TYPES, 'type');
  return (uiDecoders[type] as Decoder<UiToHub>)(o);
}
