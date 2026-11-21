import type { NodeInfo, UpdateState } from '../proto';
import { availableVersion, nodeFlag, store, updatesAvailable } from '../state';
import { useEffect, useState } from 'preact/hooks';
import { Dialog } from '../ui/Dialog';
import { countdown, NodeInstall } from './Devices';
import { Button, EmptyState, Pill, type Tone } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { clock, nodeStateLabel, nodeStateTone } from '../ui/format';
import { nodeHref } from '../ui/router';

const BUSY: Record<string, string> = { checking: 'Checking...', downloading: 'Downloading...', verifying: 'Verifying...', installing: 'Installing...', restarting: 'Restarting...' };

export function updateView(u: UpdateState | undefined): { tone: Tone; label: string } {
  if (!u) return { tone: 'muted', label: 'Not checked' };
  switch (u.state) {
    case 'up_to_date': return { tone: 'muted', label: `Up to date (checked ${clock(u.checked_ms)})` };
    case 'available': return { tone: 'info', label: `Available: ${u.version}` };
    case 'failed': return { tone: 'bad', label: `Failed: ${u.message}` };
    default: return { tone: 'info', label: BUSY[u.state]! };
  }
}

const plural = (k: number, what: string) => `${k} ${what}${k === 1 ? '' : 's'}`;

function NodeCard({ n }: { n: NodeInfo }) {
  const s = store.state.value;
  const v = s.nodeVersions[n.id];
  const flag = nodeFlag(s, n.id);
  const u = s.updates[n.id];
  const upd = updateView(u);
  const off = n.state === 'Offline';
  const working = u !== undefined && u.state in BUSY;
  const blockers = s.blockers.filter((b) => b.node === n.id).length;
  return (
    <article class="card node-card" aria-label={n.hostname}>
      <div class="card-title">
        <a class="strong" href={nodeHref(n.id)}>{n.hostname}</a>
        <Pill tone={nodeStateTone(n.state)}>{nodeStateLabel(n.state)}</Pill>
      </div>
      <dl class="details small">
        <dt>CPU</dt><dd>{off ? '-' : `${Math.round(n.telemetry.cpu_pct)}%`}</dd>
        <dt>Agents</dt><dd>{plural(n.agents.length, 'agent')}, {plural(s.nodeRepos[n.id]?.length ?? 0, 'repo')}</dd>
        <dt>Version</dt><dd class="mono">{v ? `${v.version} (${v.git.slice(0, 7)})` : 'unknown'}{flag && <> <Pill tone="warn">{flag}</Pill></>}</dd>
        <dt>Platform</dt><dd>{v ? `${v.os} ${v.arch}` : 'unknown platform'}</dd>
      </dl>
      <span class={`upd small ${upd.tone}`}>{upd.label}</span>
      {blockers > 0 && <a class="small warn-link" href="#/inbox">{plural(blockers, 'problem')} in Inbox</a>}
      <div class="row">
        {availableVersion(u)
          ? <ConfirmButton variant="primary" label="Update" confirmLabel="Restarts the node: confirm" disabled={off} onConfirm={() => store.intent({ kind: 'update_node', node: n.id })} />
          : <Button size="sm" variant="ghost" disabled={off || working} onClick={() => store.intent({ kind: 'check_update', node: n.id })}>Check</Button>}
        <span class="grow" />
        <a class="small" href={nodeHref(n.id)}>Settings and stats</a>
      </div>
    </article>
  );
}

/** Add node: a one-time code and the install command for it, in one dialog. */
function AddNode({ onClose }: { onClose(): void }) {
  const s = store.state.value;
  const o = s.otp?.kind === 'node' ? s.otp : null;
  useEffect(() => {
    store.intent({ kind: 'add_device', device: 'node' });
  }, []);
  const left = o ? o.expires_ms - s.now : 0;
  return (
    <Dialog title="Add node" onClose={onClose} actions={<Button variant="ghost" onClick={onClose}>Done</Button>}>
      <p class="small">Run this on the new machine (Linux or WSL). It installs ulakd, joins this hub with a one-time code and starts it as a service.</p>
      {!o ? <p class="muted" role="status">Creating a one-time code...</p> : (
        <>
          <span class="row small"><Pill tone={left <= 0 ? 'bad' : 'warn'}>{countdown(left)}</Pill>{left <= 0 && <Button size="sm" variant="ghost" onClick={() => store.intent({ kind: 'add_device', device: 'node' })}>New code</Button>}</span>
          <NodeInstall otp={o.otp} fp={o.hub_fingerprint} />
        </>
      )}
      <p class="muted small">Adding a phone or browser? That is under <a href="#/settings/devices">Settings › Devices</a>.</p>
    </Dialog>
  );
}

export function Nodes() {
  const s = store.state.value;
  const hub = s.hubVersion;
  const rc = s.releaseCache;
  const [adding, setAdding] = useState(false);
  const updatable = Object.values(s.updates).filter((u) => availableVersion(u) !== null).length;
  return (
    <>
      <header class="page-head">
        <h1>Nodes</h1>
        <div class="row">
          <Button variant="ghost" onClick={() => store.intent({ kind: 'check_update' })}>Check all</Button>
          <ConfirmButton variant="ghost" large label={updatable > 0 ? `Update all (${updatable})` : 'Update all'} confirmLabel="Restarts every updated node: confirm" disabled={!updatesAvailable(s)} onConfirm={() => store.intent({ kind: 'update_node' })} />
          <Button variant="primary" onClick={() => setAdding(true)}>Add node</Button>
        </div>
      </header>
      <p class="muted small">{hub ? `Hub ${hub.version} (${hub.git.slice(0, 7)}), protocol ${hub.proto}, ${hub.os} ${hub.arch}` : 'Hub version unknown'}</p>
      {rc && (
        <p class="muted small">
          {rc.latest ? `Hub release cache: ${rc.latest}` : 'Hub release cache: empty'}
          {rc.checkedMs > 0 && `, checked ${new Date(rc.checkedMs).toLocaleString()}`}
          {rc.error && <span class="err">{` (last error: ${rc.error})`}</span>}
        </p>
      )}
      {s.nodes.length === 0
        ? <EmptyState title="No nodes yet" action={<Button variant="primary" onClick={() => setAdding(true)}>Add node</Button>}>A node runs agents. Add one with a single command.</EmptyState>
        : <section class="node-grid" aria-label="Nodes">{s.nodes.map((n) => <NodeCard key={n.id} n={n} />)}</section>}
      {adding && <AddNode onClose={() => setAdding(false)} />}
    </>
  );
}
