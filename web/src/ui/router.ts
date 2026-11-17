import { signal, type ReadonlySignal } from '@preact/signals';

export const ROUTES = ['agents', 'inbox', 'nodes', 'mailbox', 'devices', 'settings'] as const;
export type Route = (typeof ROUTES)[number];

/** `#/inbox` -> 'inbox'; anything unknown falls back to 'agents'. Sub-paths (`#/agents/x`) keep the first segment. */
export function parseRoute(hash: string): Route {
  const seg = hash.replace(/^#\/?/, '').split(/[/?]/)[0] ?? '';
  return (ROUTES as readonly string[]).includes(seg) ? (seg as Route) : 'agents';
}

const current = signal<Route>(parseRoute(globalThis.location?.hash ?? ''));
if (typeof window !== 'undefined') window.addEventListener('hashchange', () => (current.value = parseRoute(location.hash)));

export const route: ReadonlySignal<Route> = current;
export const href = (r: Route): string => `#/${r}`;
export const navigate = (r: Route): void => {
  location.hash = href(r);
};
