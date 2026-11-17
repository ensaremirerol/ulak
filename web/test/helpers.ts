import type { Blocker, HubToUi, NodeInfo, UiToHub } from '../src/proto';
import { initialState, store } from '../src/state';

const tel = { load1: 0, cpu_pct: 10, gpu_pct: null, mem_used_pct: 0, at_ms: 0 };
export const node = (id: string, hostname: string, agents: string[] = [], state: NodeInfo['state'] = 'Idle'): NodeInfo =>
  ({ id, hostname, state, telemetry: tel, agents });
export const blocker = (id: string, n: string, over: Partial<Blocker> = {}): Blocker =>
  ({ id, node: n, profile: 'claude', kind: 'Login', agent: 'a1', detail: 'run claude /login', at_ms: 1, ...over });

/** Reset the global store, install a capturing sender, return the captured frames. */
export function freshStore(conn: 'open' | 'connecting' = 'open'): UiToHub[] {
  const sent: UiToHub[] = [];
  (store.state as { value: unknown }).value = { ...initialState(1_000_000), conn };
  store.setSender((f) => (sent.push(f), 'sent'));
  return sent;
}

export const feed = (...frames: HubToUi[]) => frames.forEach((f) => store.onFrame(f));
export const ofType = <T extends UiToHub['type']>(sent: UiToHub[], t: T) => sent.filter((f) => f.type === t);
