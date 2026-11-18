import { decodeHubToUi, encodeUiToHub, type HubToUi, type UiToHub, type UnknownFrame } from '../proto';
import { backoffDelay } from './backoff';
import type { Probe } from './session';

/** Connection state machine: connecting -> open -> reconnecting -> open ...; unauthorized is terminal until `retry()`. */
export type ConnState = 'connecting' | 'open' | 'reconnecting' | 'unauthorized';

export interface SocketLike {
  readyState: number;
  onopen: ((e: unknown) => void) | null;
  onmessage: ((e: { data: unknown }) => void) | null;
  onclose: ((e: { code?: number }) => void) | null;
  onerror: ((e: unknown) => void) | null;
  send(data: string): void;
  close(code?: number): void;
}

export interface Timers {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(h: unknown): void;
  now(): number;
  random(): number;
}

export interface ClientOptions {
  url(): string;
  onFrame(f: HubToUi): void;
  onState(s: ConnState): void;
  /** Frames to send first after every (re)connect: the hub forgets attachments with the socket. */
  resync(): UiToHub[];
  probe(): Promise<Probe>;
  createSocket?(url: string): SocketLike;
  timers?: Timers;
  env?: PageEnv;
}

/** The page's visibility and network events (a seam for tests). `listen` returns an unsubscribe. */
export interface PageEnv {
  listen(event: 'visibilitychange' | 'online', fn: () => void): () => void;
  visible(): boolean;
}

const pageEnv: PageEnv = {
  listen(event, fn) {
    const target: EventTarget = event === 'online' ? window : document;
    target.addEventListener(event, fn);
    return () => target.removeEventListener(event, fn);
  },
  visible: () => document.visibilityState === 'visible',
};

const OPEN = 1;
export const QUEUE_MAX = 20;
/** Silence this long on an open socket triggers an app-level ping (`list_devices`). */
export const PING_AFTER_MS = 25_000;
/** ... and no frame at all this long after the ping closes the socket and reconnects. */
export const PONG_WITHIN_MS = 10_000;

/** Frames that are idempotent reads: queued while offline and sent once on reconnect. Never keystrokes. */
export function isQueueable(f: UiToHub): boolean {
  return f.type === 'list_devices' || f.type === 'mailbox_log' || f.type === 'check_update' || f.type === 'node_stats';
}

const realTimers: Timers = {
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (h) => clearTimeout(h as number),
  now: () => Date.now(),
  random: () => Math.random(),
};

export interface Client {
  start(): void;
  stop(): void;
  /** Reconnect now (resets the backoff); also leaves `unauthorized`. */
  retry(): void;
  /** `sent`: on the wire; `queued`: held until reconnect; `dropped`: unsafe to hold, tell the user. */
  send(f: UiToHub): 'sent' | 'queued' | 'dropped';
  readonly state: ConnState;
}

export function createClient(o: ClientOptions): Client {
  const t = o.timers ?? realTimers;
  const make = o.createSocket ?? ((u: string) => new WebSocket(u) as unknown as SocketLike);
  let state: ConnState = 'connecting';
  let ws: SocketLike | null = null;
  let attempt = 0;
  let retryTimer: unknown = null;
  let pingTimer: unknown = null;
  let deadTimer: unknown = null;
  let lastRx = 0;
  let running = false;
  let queue: UiToHub[] = [];

  const set = (s: ConnState) => {
    if (s === state) return;
    state = s;
    o.onState(s);
  };
  const clearTimers = () => {
    for (const h of [retryTimer, pingTimer, deadTimer]) if (h !== null) t.clearTimeout(h);
    retryTimer = pingTimer = deadTimer = null;
  };

  const wire = (f: UiToHub) => ws?.send(encodeUiToHub(f));

  function armPing() {
    if (pingTimer !== null) t.clearTimeout(pingTimer);
    pingTimer = t.setTimeout(() => {
      if (!ws || ws.readyState !== OPEN) return;
      wire({ type: 'list_devices' });
      deadTimer = t.setTimeout(() => ws?.close(4000), PONG_WITHIN_MS);
    }, PING_AFTER_MS);
  }

  function connect() {
    if (!running) return;
    clearTimers();
    let opened = false;
    const sock = make(o.url());
    ws = sock;
    sock.onopen = () => {
      if (ws !== sock) return;
      opened = true;
      attempt = 0;
      lastRx = t.now();
      for (const f of o.resync()) wire(f);
      const held = queue;
      queue = [];
      for (const f of held) wire(f);
      set('open');
      armPing();
    };
    sock.onmessage = (e) => {
      if (ws !== sock || typeof e.data !== 'string') return;
      lastRx = t.now();
      if (deadTimer !== null) t.clearTimeout(deadTimer);
      deadTimer = null;
      armPing();
      const f: HubToUi | UnknownFrame = decodeHubToUi(e.data);
      if (f.type !== 'unknown') o.onFrame(f);
    };
    sock.onerror = () => {};
    sock.onclose = () => {
      if (ws !== sock) return;
      ws = null;
      clearTimers();
      if (!running) return;
      if (opened) return scheduleRetry();
      // Never opened: a refused upgrade (401/403) looks like any network failure to the browser.
      set('reconnecting');
      void o.probe().then((p) => {
        if (!running || ws) return;
        if (p === 'unauthorized') set('unauthorized');
        else scheduleRetry();
      });
    };
  }

  function scheduleRetry() {
    set('reconnecting');
    retryTimer = t.setTimeout(connect, backoffDelay(attempt++, t.random));
  }

  /** Visible again / network back: do not wait out the backoff. */
  function wake() {
    if (!running || state === 'unauthorized') return;
    if (state === 'reconnecting' && !ws) {
      attempt = 0;
      connect();
    } else if (state === 'open' && ws && t.now() - lastRx > PING_AFTER_MS) {
      wire({ type: 'list_devices' });
      if (deadTimer === null) deadTimer = t.setTimeout(() => ws?.close(4000), PONG_WITHIN_MS);
    }
  }

  const env = o.env ?? pageEnv;
  let unlisten: Array<() => void> = [];
  const onVis = () => {
    if (env.visible()) wake();
  };

  return {
    get state() {
      return state;
    },
    start() {
      if (running) return;
      running = true;
      unlisten = [env.listen('visibilitychange', onVis), env.listen('online', wake)];
      connect();
    },
    stop() {
      running = false;
      for (const u of unlisten) u();
      unlisten = [];
      clearTimers();
      const s = ws;
      ws = null;
      s?.close(1000);
    },
    retry() {
      if (!running) return;
      attempt = 0;
      const s = ws;
      ws = null;
      s?.close(1000);
      set('connecting');
      connect();
    },
    send(f) {
      if (ws && ws.readyState === OPEN && state === 'open') {
        wire(f);
        return 'sent';
      }
      if (!isQueueable(f)) return 'dropped';
      const json = encodeUiToHub(f);
      if (!queue.some((q) => encodeUiToHub(q) === json)) queue.push(f);
      if (queue.length > QUEUE_MAX) queue.shift();
      return 'queued';
    },
  };
}
