import type { NodeInfo, UpdateState } from '../proto';
import { availableVersion, nodeFlag, store, updatesAvailable } from '../state';
import { BlockerCard } from '../ui/BlockerCard';
import { Button, EmptyState, Pill, type Tone } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { clock, nodeStateLabel, nodeStateTone } from '../ui/format';

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

function Row({ n }: { n: NodeInfo }) {
  const s = store.state.value;
  const v = s.nodeVersions[n.id];
  const flag = nodeFlag(s, n.id);
  const u = s.updates[n.id];
  const upd = updateView(u);
  const off = n.state === 'Offline';
  const working = u !== undefined && u.state in BUSY;
  return (
    <tr>
      <td><div class="strong">{n.hostname}</div><div class="muted small">{v ? `${v.os} ${v.arch}` : 'unknown platform'}</div></td>
      <td><Pill tone={nodeStateTone(n.state)}>{nodeStateLabel(n.state)}</Pill></td>
      <td class="mono">{v ? `${v.version} (${v.git.slice(0, 7)})` : 'unknown'}{flag && <> <Pill tone="warn">{flag}</Pill></>}</td>
      <td><span class={`upd ${upd.tone}`}>{upd.label}</span></td>
      <td class="actions">
        <Button size="sm" variant="ghost" disabled={off || working} onClick={() => store.intent({ kind: 'check_update', node: n.id })}>Check</Button>
        {availableVersion(u) && <ConfirmButton variant="primary" label="Update" confirmLabel="Restarts the node: confirm" disabled={off} onConfirm={() => store.intent({ kind: 'update_node', node: n.id })} />}
      </td>
    </tr>
  );
}

export function Nodes() {
  const s = store.state.value;
  const hub = s.hubVersion;
  const rc = s.releaseCache;
  return (
    <>
      <header class="page-head">
        <h1>Nodes</h1>
        <div class="row">
          <Button variant="ghost" onClick={() => store.intent({ kind: 'check_update' })}>Check all</Button>
          <ConfirmButton variant="ghost" large label="Update all" confirmLabel="Restarts every updated node: confirm" disabled={!updatesAvailable(s)} onConfirm={() => store.intent({ kind: 'update_node' })} />
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
      {s.blockers.length > 0 && <section class="stack" aria-label="Needs attention">{s.blockers.map((b) => <BlockerCard key={b.id} b={b} />)}</section>}
      {s.nodes.length === 0 ? <EmptyState title="No nodes yet">Enrol one from the Devices page.</EmptyState> : (
        <section class="table-wrap" aria-label="Nodes">
          <table>
            <thead><tr><th>Node</th><th>State</th><th>Version</th><th>Update</th><th class="actions">Actions</th></tr></thead>
            <tbody>{s.nodes.map((n) => <Row key={n.id} n={n} />)}</tbody>
          </table>
        </section>
      )}
    </>
  );
}
