import { describe, expect, it, vi } from 'vitest';
import {
  backoffDelay, BACKOFF_CAP_MS, createClient, PING_AFTER_MS, PONG_WITHIN_MS, QUEUE_MAX,
  takeTokenFromLocation, setToken, wsUrl, probeSession, type ConnState, type PageEnv, type SocketLike, type Timers,
} from '../src/net';
import type { HubToUi, UiToHub } from '../src/proto';

class FakeSocket implements SocketLike {
  readyState = 0;
  onopen: SocketLike['onopen'] = null;
  onmessage: SocketLike['onmessage'] = null;
  onclose: SocketLike['onclose'] = null;
  onerror: SocketLike['onerror'] = null;
  sent: string[] = [];
  closed: number | undefined;
  constructor(public url: string) {}
  send(d: string) { this.sent.push(d); }
  close(code?: number) { this.closed = code; this.readyState = 3; }
  open() { this.readyState = 1; this.onopen?.({}); }
  recv(d: unknown) { this.onmessage?.({ data: typeof d === 'string' ? d : JSON.stringify(d) }); }
  drop() { this.readyState = 3; this.onclose?.({ code: 1006 }); }
}

function rig(probe: () => Promise<'ok' | 'unauthorized' | 'error'> = async () => 'error') {
  let now = 0;
  const timers: { at: number; fn: () => void; id: number }[] = [];
  let nid = 0;
  const T: Timers = {
    setTimeout: (fn, ms) => { const id = ++nid; timers.push({ at: now + ms, fn, id }); return id; },
    clearTimeout: (h) => { const i = timers.findIndex((x) => x.id === h); if (i >= 0) timers.splice(i, 1); },
    now: () => now,
    random: () => 1,
  };
  const advance = (ms: number) => {
    const end = now + ms;
    for (;;) {
      timers.sort((a, b) => a.at - b.at);
      const n = timers[0];
      if (!n || n.at > end) break;
      timers.shift();
      now = n.at;
      n.fn();
    }
    now = end;
  };
  const socks: FakeSocket[] = [];
  const states: ConnState[] = [];
  const frames: HubToUi[] = [];
  const listeners: Record<string, () => void> = {};
  let visible = true;
  const env: PageEnv = {
    listen: (e, fn) => { listeners[e] = fn; return () => { delete listeners[e]; }; },
    visible: () => visible,
  };
  let resync: UiToHub[] = [];
  const c = createClient({
    url: () => 'ws://x/ws/ui',
    onFrame: (f) => frames.push(f),
    onState: (s) => states.push(s),
    resync: () => resync,
    probe,
    createSocket: (u) => { const s = new FakeSocket(u); socks.push(s); return s; },
    timers: T,
    env,
  });
  return {
    c, socks, states, frames, advance, listeners,
    setVisible: (v: boolean) => { visible = v; },
    setResync: (r: UiToHub[]) => { resync = r; },
    last: () => socks[socks.length - 1]!,
    flush: () => new Promise((r) => setTimeout(r, 0)),
  };
}

describe('backoff', () => {
  it('doubles, is capped and jittered between 50% and 100%', () => {
    expect(backoffDelay(0, () => 1)).toBe(500);
    expect(backoffDelay(1, () => 1)).toBe(1000);
    expect(backoffDelay(3, () => 0)).toBe(2000);
    expect(backoffDelay(50, () => 1)).toBe(BACKOFF_CAP_MS);
    expect(backoffDelay(50, () => 0)).toBe(BACKOFF_CAP_MS / 2);
  });
});

describe('client', () => {
  it('connects, reports open, sends resync frames first and decodes frames', () => {
    const r = rig();
    r.setResync([{ type: 'attach', agent: 'a-1' }]);
    r.c.start();
    expect(r.c.state).toBe('connecting');
    r.last().open();
    expect(r.states).toEqual(['open']);
    expect(r.last().sent).toEqual(['{"type":"attach","agent":"a-1"}']);
    r.last().recv({ type: 'error', message: 'x' });
    r.last().recv('garbage');
    r.last().recv({ type: 'from_the_future' });
    expect(r.frames).toEqual([{ type: 'error', message: 'x' }]);
  });

  it('reconnects with growing delays and resets after success', () => {
    const r = rig();
    r.c.start();
    r.last().open();
    r.last().drop();
    expect(r.c.state).toBe('reconnecting');
    r.advance(499);
    expect(r.socks).toHaveLength(1);
    r.advance(1);
    expect(r.socks).toHaveLength(2);
    r.last().drop(); // never opened: probe says 'error' -> retry, delay 1000
    return r.flush().then(() => {
      r.advance(999);
      expect(r.socks).toHaveLength(2);
      r.advance(1);
      expect(r.socks).toHaveLength(3);
      r.last().open();
      r.last().drop();
      r.advance(500);
      expect(r.socks).toHaveLength(4);
    });
  });

  it('a refused upgrade is probed and 401 becomes unauthorized without retrying', async () => {
    const probe = vi.fn(async () => 'unauthorized' as const);
    const r = rig(probe);
    r.c.start();
    r.last().drop();
    await r.flush();
    expect(probe).toHaveBeenCalledOnce();
    expect(r.c.state).toBe('unauthorized');
    r.advance(120_000);
    expect(r.socks).toHaveLength(1);
    r.c.retry();
    expect(r.socks).toHaveLength(2);
    expect(r.c.state).toBe('connecting');
  });

  it('does not probe after a socket that was open (just reconnects)', () => {
    const probe = vi.fn(async () => 'unauthorized' as const);
    const r = rig(probe);
    r.c.start();
    r.last().open();
    r.last().drop();
    expect(probe).not.toHaveBeenCalled();
  });

  it('queues only safe reads while offline and flushes them (deduped) after resync', () => {
    const r = rig();
    r.c.start();
    expect(r.c.send({ type: 'list_devices' })).toBe('queued');
    expect(r.c.send({ type: 'list_devices' })).toBe('queued');
    expect(r.c.send({ type: 'mailbox_log', thread: null, limit: 5 })).toBe('queued');
    expect(r.c.send({ type: 'pane_input', agent: 'a', data: new Uint8Array([97]) })).toBe('dropped');
    expect(r.c.send({ type: 'kill', agent: 'a' })).toBe('dropped');
    expect(r.c.send({ type: 'answer', question: 'q', text: 'y' })).toBe('dropped');
    r.setResync([{ type: 'attach', agent: 'a' }]);
    r.last().open();
    expect(r.last().sent.map((s) => JSON.parse(s).type)).toEqual(['attach', 'list_devices', 'mailbox_log']);
    expect(r.c.send({ type: 'kill', agent: 'a' })).toBe('sent');
  });

  it('keeps the offline queue bounded', () => {
    const r = rig();
    r.c.start();
    for (let i = 0; i < QUEUE_MAX + 5; i++) r.c.send({ type: 'mailbox_log', thread: null, limit: i + 1 });
    r.last().open();
    expect(r.last().sent).toHaveLength(QUEUE_MAX);
  });

  it('reconnects at once when the tab becomes visible or the network returns', () => {
    const r = rig();
    r.c.start();
    r.last().open();
    r.last().drop();
    r.advance(1); // still waiting out the backoff
    expect(r.socks).toHaveLength(1);
    r.setVisible(false);
    r.listeners.visibilitychange?.();
    expect(r.socks).toHaveLength(1);
    r.setVisible(true);
    r.listeners.visibilitychange?.();
    expect(r.socks).toHaveLength(2);
    r.last().drop();
    return r.flush().then(() => {
      r.listeners.online?.();
      expect(r.socks).toHaveLength(3);
    });
  });

  it('pings after silence and drops a dead socket', () => {
    const r = rig();
    r.c.start();
    r.last().open();
    r.advance(PING_AFTER_MS);
    expect(r.last().sent.map((s) => JSON.parse(s).type)).toEqual(['list_devices']);
    r.advance(PONG_WITHIN_MS);
    expect(r.socks[0]!.closed).toBe(4000);
  });

  it('any frame keeps the connection alive', () => {
    const r = rig();
    r.c.start();
    r.last().open();
    r.advance(PING_AFTER_MS);
    r.last().recv({ type: 'devices', devices: [] });
    r.advance(PONG_WITHIN_MS + 1);
    expect(r.socks[0]!.closed).toBeUndefined();
  });

  it('stop closes the socket and cancels retries', () => {
    const r = rig();
    r.c.start();
    r.last().open();
    r.c.stop();
    expect(r.socks[0]!.closed).toBe(1000);
    r.advance(100_000);
    expect(r.socks).toHaveLength(1);
  });
});

describe('session helpers', () => {
  it('takes the token from the fragment or query, strips it and builds the ws url', () => {
    const replace = vi.fn();
    const loc = { hash: '#token=abc%20d&x=1', search: '?a=1', pathname: '/' } as Location;
    expect(takeTokenFromLocation(loc, { replaceState: replace } as unknown as History)).toBe(true);
    expect(replace).toHaveBeenCalledWith(null, '', '/?a=1#x=1');
    expect(wsUrl({ protocol: 'https:', host: 'h.example' })).toBe('wss://h.example/ws/ui?token=abc%20d');
    setToken(null);
    expect(wsUrl({ protocol: 'http:', host: 'localhost:8080' })).toBe('ws://localhost:8080/ws/ui');
    const loc2 = { hash: '', search: '?token=zz', pathname: '/app' } as Location;
    expect(takeTokenFromLocation(loc2, { replaceState: replace } as unknown as History)).toBe(true);
    expect(replace).toHaveBeenLastCalledWith(null, '', '/app');
    const loc3 = { hash: '#/agents', search: '?token=q', pathname: '/' } as Location;
    expect(takeTokenFromLocation(loc3, { replaceState: replace } as unknown as History)).toBe(true);
    expect(replace).toHaveBeenLastCalledWith(null, '', '/#/agents');
    expect(takeTokenFromLocation({ hash: '#/agents', search: '', pathname: '/' } as Location)).toBe(false);
    setToken(null);
  });

  it('probe maps 401/403 to unauthorized and network errors to error', async () => {
    const f = (s: number) => (async () => new Response('', { status: s })) as unknown as typeof fetch;
    expect(await probeSession(f(401))).toBe('unauthorized');
    expect(await probeSession(f(403))).toBe('unauthorized');
    expect(await probeSession(f(200))).toBe('ok');
    expect(await probeSession(f(502))).toBe('error');
    expect(await probeSession((async () => { throw new TypeError('net'); }) as unknown as typeof fetch)).toBe('error');
  });
});
