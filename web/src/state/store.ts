import { computed, signal, type ReadonlySignal } from '@preact/signals';
import type { HubToUi, UiToHub } from '../proto';
import { applyIntentLocally, IntentError, intentToFrames } from './intents';
import { awaitsSession, initialState, reduce } from './reducer';
import type { Event, Intent, State } from './types';

/** How the store reaches the network (see net/client.ts `send`). */
export type Sender = (f: UiToHub) => 'sent' | 'queued' | 'dropped';

export interface Store {
  /** The whole state. Read `.value` in components; use `select` for a derived, deduplicated slice. */
  readonly state: ReadonlySignal<State>;
  select<T>(pick: (s: State) => T): ReadonlySignal<T>;
  /** Apply a connection or hub event. */
  dispatch(e: Event): void;
  /** Apply a hub frame (shorthand for the net layer). */
  onFrame(f: HubToUi): void;
  /** Turn an intent into frames, send them, apply local effects. Returns false when nothing went out. */
  intent(i: Intent): boolean;
  toast(kind: 'error' | 'info' | 'ok', text: string): void;
  /** Frames to replay right after a reconnect (re-attach the selected agent). */
  resync(): UiToHub[];
  /** Be told about every pane chunk (e.g. a terminal emulator that writes incrementally). */
  onPane(agent: string, fn: (data: Uint8Array) => void): () => void;
  setSender(s: Sender): void;
}

export function createStore(init: State = initialState()): Store {
  const state = signal<State>(init);
  let send: Sender = () => 'dropped';
  const panes = new Map<string, Set<(d: Uint8Array) => void>>();

  const dispatch = (e: Event) => {
    const before = state.value;
    state.value = reduce(before, e);
    // The selected agent just got its session: send the attach that select held back.
    const sel = state.value.selected;
    if (sel && sel === before.selected && awaitsSession(before.statuses[sel]) && state.value.statuses[sel] === 'Running') {
      send({ type: 'attach', agent: sel });
    }
    if (e.kind === 'hub' && e.frame.type === 'pane_output') {
      for (const fn of panes.get(e.frame.agent) ?? []) fn(e.frame.data);
    }
  };

  const store: Store = {
    state,
    select: (pick) => computed(() => pick(state.value)),
    dispatch,
    onFrame: (frame) => dispatch({ kind: 'hub', frame }),
    toast: (toast, text) => dispatch({ kind: 'toast', toast, text }),
    resync: () => [
      { type: 'list_credentials' },
      { type: 'list_queue' },
      ...(state.value.selected ? [{ type: 'attach', agent: state.value.selected } as const] : []),
    ],
    onPane(agent, fn) {
      const set = panes.get(agent) ?? new Set();
      set.add(fn);
      panes.set(agent, set);
      return () => set.delete(fn);
    },
    setSender: (s) => {
      send = s;
    },
    intent(i) {
      let frames: UiToHub[];
      try {
        frames = intentToFrames(state.value, i);
      } catch (e) {
        if (!(e instanceof IntentError)) throw e;
        dispatch({ kind: 'toast', toast: 'error', text: e.message });
        return false;
      }
      const results = frames.map(send);
      if (results.includes('dropped')) {
        // Never half-apply: nothing local happens and the user hears about it. Keystrokes are never held.
        dispatch({ kind: 'toast', toast: 'error', text: state.value.conn === 'unauthorized'
          ? 'You are signed out. Sign in again, then retry.'
          : 'Not connected to the hub: that action was not sent. Try again once it says Connected.' });
        return false;
      }
      state.value = applyIntentLocally(state.value, i);
      return frames.length > 0 || i.kind === 'detach' || i.kind === 'select';
    },
  };
  return store;
}

/** The app-wide store (tests build their own with `createStore`). */
export const store: Store = createStore();
