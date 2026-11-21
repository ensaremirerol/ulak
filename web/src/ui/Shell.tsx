import type { ComponentChildren } from 'preact';
import { updateReload } from '../pwa/register';
import { store, waitingForwards } from '../state';
import { Badge, Button, Dot } from './components';
import { NAV, href, route, type Route } from './router';
import { ToastHost } from './Toast';

const TITLES: Record<Route, string> = { agents: 'Agents', issues: 'Issues', inbox: 'Inbox', nodes: 'Nodes', mailbox: 'Mailbox', devices: 'Devices', settings: 'Settings' };
const ICONS: Record<Route, string> = {
  agents: 'M4 6h16M4 12h16M4 18h10', issues: 'M12 3a9 9 0 100 18 9 9 0 000-18zM12 8v5M12 16h.01', inbox: 'M4 5h16v11H8l-4 4z', nodes: 'M4 5h16v6H4zM4 13h16v6H4z',
  mailbox: 'M3 7l9 6 9-6M3 7v10h18V7z', devices: 'M7 4h10v16H7zM11 17h2', settings: 'M12 8a4 4 0 100 8 4 4 0 000-8z',
};
export const routeTitle = (r: Route): string => TITLES[r];

const CONN = { open: ['Connected', 'ok'], connecting: ['Connecting', 'info'], reconnecting: ['Reconnecting', 'warn'], unauthorized: ['Signed out', 'bad'] } as const;

/** Sidebar on desktop, bottom tabs on phones (CSS only), connection indicator, offline banner, toasts. */
export function Shell({ children }: { children: ComponentChildren }) {
  const s = store.state.value;
  const [label, tone] = CONN[s.conn];
  const waiting = s.questions.length + s.blockers.length + waitingForwards(s).length;
  const reload = updateReload.value;
  return (
    <div class="shell">
      <nav class="sidebar" aria-label="Main">
        <div class="brand">
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 12 L20 4 L14 20 L11 13 Z" /></svg>
          <span>ulak</span>
        </div>
        <div class="nav">
          {NAV.map((r) => (
            <a key={r} href={href(r)} aria-current={route.value === r || (r === 'settings' && route.value === 'devices') || (r === 'inbox' && route.value === 'mailbox') ? 'page' : undefined}>
              <span class="nav-label">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={ICONS[r]} /></svg>
                {TITLES[r]}
              </span>
              {r === 'inbox' && <Badge count={waiting} />}
              {r === 'agents' && <Badge count={s.queue.length} />}
            </a>
          ))}
        </div>
        <div class="spacer" />
        <div class="side-foot">
          <span>{s.hubVersion ? `hub ${s.hubVersion.version}` : 'hub'}</span>
          <span class="conn" role="status"><Dot tone={tone} />{label}</span>
        </div>
      </nav>
      <main class="main" id="main">
        {s.conn !== 'open' && s.conn !== 'unauthorized' && <div class="banner" role="status">Offline: showing the last known state. Actions are not sent until the connection is back.</div>}
        {reload && <div class="banner" role="status">New version available <Button size="sm" variant="primary" onClick={reload}>Reload</Button></div>}
        <span class="conn-mobile"><Dot tone={tone} />{label}</span>
        {children}
      </main>
      <ToastHost toasts={s.toasts} onDismiss={(id) => store.dispatch({ kind: 'dismiss_toast', id })} />
    </div>
  );
}
