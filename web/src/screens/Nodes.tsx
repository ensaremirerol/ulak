import type { CleanItem, NodeInfo, RepoEntry, UpdateState } from '../proto';
import { availableVersion, nodeFlag, store, updatesAvailable } from '../state';
import { BlockerCard } from '../ui/BlockerCard';
import { Button, EmptyState, Pill, type Tone } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { ageLabel, clock, nodeStateLabel, nodeStateTone } from '../ui/format';

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

const syncLabel = (r: RepoEntry): string =>
  r.ahead === null || r.behind === null ? 'no upstream' : r.ahead === 0 && r.behind === 0 ? 'in sync' : `${r.ahead} ahead, ${r.behind} behind`;

function Items({ title, items }: { title: string; items: CleanItem[] }) {
  return (
    <div class="stack tight">
      <strong class="small">{title} ({items.length})</strong>
      {items.length > 0 && <ul class="repo-list small">{items.map((i) => <li key={i.path}><span class="mono">{i.full_name ?? i.path}</span><span class="muted">{i.reason}</span></li>)}</ul>}
    </div>
  );
}

/** A node's profiles, its repo index, cleanup buttons and the last clean report. */
function NodeRepos({ n }: { n: NodeInfo }) {
  const s = store.state.value;
  const profiles = s.nodeProfiles[n.id] ?? [];
  const repos = s.nodeRepos[n.id] ?? [];
  const report = s.lastClean[n.id];
  const off = n.state === 'Offline';
  return (
    <section class="card stack" aria-label={`Repositories on ${n.hostname}`}>
      <div class="card-title"><strong>{n.hostname}</strong></div>
      {profiles.length > 0 && (
        <ul class="repo-list small" aria-label={`Profiles on ${n.hostname}`}>
          {profiles.map((p) => (
            <li key={p.name}>
              <span class="strong">{p.name}</span>
              <span class="muted mono">{p.config_dir ?? 'default config'}</span>
              <Pill tone={p.installed ? 'ok' : 'muted'}>{p.installed ? 'Installed' : 'Not installed'}</Pill>
              {p.login_blocked && <Pill tone="warn">Login required</Pill>}
            </li>
          ))}
        </ul>
      )}
      <details>
        <summary>Repositories ({repos.length})</summary>
        {repos.length === 0 ? <p class="muted small">No repositories indexed on this node.</p> : (
          <ul class="repo-list small" aria-label={`Repositories of ${n.hostname}`}>
            {repos.map((r) => (
              <li key={r.path}>
                <span class="strong">{r.full_name}</span>
                <span class="mono">{r.branch ?? 'detached'}</span>
                <Pill tone={r.clean ? 'ok' : 'warn'}>{r.clean ? 'Clean' : 'Dirty'}</Pill>
                <span class="muted">{syncLabel(r)}</span>
                <span class="muted">{r.last_fetch_ms === null ? 'never fetched' : `fetched ${ageLabel(Math.max(0, s.now - r.last_fetch_ms))}`}</span>
                {r.in_use && <Pill tone="info">In use</Pill>}
                <span class="muted">{r.managed ? 'managed' : 'external'}</span>
                {r.managed && !r.in_use && <ConfirmButton label="Remove" confirmLabel="Click again to remove" disabled={off} onConfirm={() => store.intent({ kind: 'clean_repo', node: n.id, path: r.path })} />}
              </li>
            ))}
          </ul>
        )}
      </details>
      <div class="row">
        <Button size="sm" variant="ghost" disabled={off} onClick={() => store.intent({ kind: 'clean_node', node: n.id, dryRun: true })}>Preview cleanup</Button>
        <ConfirmButton label="Clean up" confirmLabel="Deletes unused clones: confirm" disabled={off} onConfirm={() => store.intent({ kind: 'clean_node', node: n.id, dryRun: false })} />
      </div>
      {report && (
        <div class="stack tight" role="status" aria-label={`Cleanup report for ${n.hostname}`}>
          <Items title={report.dryRun ? 'Would remove' : 'Removed'} items={report.removed} />
          <Items title="Kept" items={report.kept} />
        </div>
      )}
    </section>
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
      {s.nodes.length > 0 && <div class="stack">{s.nodes.map((n) => <NodeRepos key={n.id} n={n} />)}</div>}
    </>
  );
}
