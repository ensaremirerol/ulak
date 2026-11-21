import { signal, type ReadonlySignal } from '@preact/signals';

export const ROUTES = ['agents', 'issues', 'inbox', 'nodes', 'mailbox', 'devices', 'settings'] as const;
/** The main navigation (Mailbox and Devices are reached from Inbox and Settings). */
export const NAV = ['agents', 'issues', 'inbox', 'nodes', 'settings'] as const satisfies readonly Route[];
export type Route = (typeof ROUTES)[number];

/** `#/inbox` -> 'inbox'; anything unknown falls back to 'agents'. Sub-paths (`#/agents/x`) keep the first segment. */
export function parseRoute(hash: string): Route {
  const seg = hash.replace(/^#\/?/, '').split(/[/?]/)[0] ?? '';
  return (ROUTES as readonly string[]).includes(seg) ? (seg as Route) : 'agents';
}

/** `#/nodes/<id>` -> the node id (URL-decoded); null for anything else, including a malformed escape. */
export function parseNodeId(hash: string): string | null {
  const m = /^#?\/?nodes\/([^/?]+)/.exec(hash);
  if (!m) return null;
  try {
    return decodeURIComponent(m[1]!);
  } catch {
    return null;
  }
}

/** The second path segment (`#/settings/templates` -> 'templates'), or null. */
export function parseSub(hash: string): string | null {
  return hash.replace(/^#\/?/, '').split(/[?]/)[0]!.split('/')[1] || null;
}

const current = signal<Route>(parseRoute(globalThis.location?.hash ?? ''));
const currentNode = signal<string | null>(parseNodeId(globalThis.location?.hash ?? ''));
const currentSub = signal<string | null>(parseSub(globalThis.location?.hash ?? ''));
if (typeof window !== 'undefined') {
  window.addEventListener('hashchange', () => {
    current.value = parseRoute(location.hash);
    currentNode.value = parseNodeId(location.hash);
    currentSub.value = parseSub(location.hash);
  });
}
/** The sub page of the current route (Settings tabs). */
export const routeSub: ReadonlySignal<string | null> = currentSub;

export const route: ReadonlySignal<Route> = current;
/** The node of a node detail route (`#/nodes/<id>`), else null. */
export const routeNode: ReadonlySignal<string | null> = currentNode;
export const nodeHref = (id: string): string => `#/nodes/${encodeURIComponent(id)}`;
/** The separate-window terminal of one agent (opened by "New window"). */
export const termHref = (agent: string): string => `#/term/${encodeURIComponent(agent)}`;
/** `#/term/<agent>` -> the agent id; null otherwise. Read once: that window shows nothing else. */
export function parseTermAgent(hash: string): string | null {
  const m = /^#?\/?term\/([^/?]+)/.exec(hash);
  if (!m) return null;
  try {
    return decodeURIComponent(m[1]!);
  } catch {
    return null;
  }
}
export const href = (r: Route): string => `#/${r}`;
export const navigate = (r: Route): void => {
  location.hash = href(r);
};
