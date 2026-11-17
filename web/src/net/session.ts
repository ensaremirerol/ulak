// The browser session is a cookie (HttpOnly, invisible to us). The only token we ever hold is one
// given in the URL (`#token=...` or `?token=...`, for contexts that cannot keep the cookie). It
// lives in memory only and is stripped from the address bar.
let token: string | null = null;

export const getToken = (): string | null => token;
export const setToken = (t: string | null): void => {
  token = t;
};

/** Reads and removes `token` from the URL fragment or query. Returns whether one was found. */
export function takeTokenFromLocation(loc: Location = location, hist: History = history): boolean {
  const frag = new URLSearchParams(loc.hash.replace(/^#\??/, ''));
  const query = new URLSearchParams(loc.search);
  const found = frag.get('token') ?? query.get('token');
  if (!found) return false;
  token = found;
  const inHash = frag.has('token');
  frag.delete('token');
  query.delete('token');
  const search = query.toString();
  // Re-serialising a route hash like `#/agents` would mangle it, so only touch it when it held the token.
  const hash = inHash ? frag.toString() : loc.hash.replace(/^#/, '');
  hist.replaceState(null, '', loc.pathname + (search ? `?${search}` : '') + (hash ? `#${hash}` : ''));
  return true;
}

export function wsUrl(loc: Pick<Location, 'protocol' | 'host'> = location): string {
  const base = `${loc.protocol === 'https:' ? 'wss:' : 'ws:'}//${loc.host}/ws/ui`;
  return token ? `${base}?token=${encodeURIComponent(token)}` : base;
}

export function authHeaders(): Record<string, string> {
  return token ? { Authorization: `Bearer ${token}` } : {};
}

export type Probe = 'ok' | 'unauthorized' | 'error';

/** GET /api/version tells a refused upgrade (401/403: log in) from a network failure. */
export async function probeSession(f: typeof fetch = fetch): Promise<Probe> {
  try {
    const r = await f('/api/version', { credentials: 'same-origin', cache: 'no-store', headers: authHeaders() });
    if (r.status === 401 || r.status === 403) return 'unauthorized';
    return r.ok ? 'ok' : 'error';
  } catch {
    return 'error';
  }
}
