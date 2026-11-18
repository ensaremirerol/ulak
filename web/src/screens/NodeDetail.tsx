import { useEffect, useId, useState } from 'preact/hooks';
import type { CleanItem, NodeInfo, NodeSettings, RepoEntry, Telemetry } from '../proto';
import { agentStatusLabel, agentTone, availableVersion, settingsErrors, settingsToForm, store, type SettingsForm, type Tri } from '../state';
import { Button, Card, EmptyState, Pill, ProgressBar, TextField } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { ageLabel, clock, day, nodeStateLabel, nodeStateTone } from '../ui/format';
import { href } from '../ui/router';
import { Segmented } from '../ui/Segmented';
import { HubProfiles } from './HubProfiles';
import { updateView } from './Nodes';

// ---- chart: plain SVG, user units, stretched to the box (strokes keep their width) ----

const W = 600;
const H = 120;
type Pick = (t: Telemetry) => number | null;
const SERIES: { key: string; label: string; pick: Pick }[] = [
  { key: 'cpu', label: 'CPU', pick: (t) => t.cpu_pct },
  { key: 'gpu', label: 'GPU', pick: (t) => t.gpu_pct },
  { key: 'mem', label: 'Memory', pick: (t) => t.mem_used_pct },
];

/** Path through the samples; a missing value (no GPU) breaks the line. */
export function linePath(samples: readonly Telemetry[], pick: Pick): string {
  const t0 = samples[0]?.at_ms ?? 0;
  const span = Math.max(1, (samples[samples.length - 1]?.at_ms ?? 0) - t0);
  let d = '';
  let pen = false;
  for (const t of samples) {
    const v = pick(t);
    if (v === null) { pen = false; continue; }
    const x = ((t.at_ms - t0) / span) * W;
    const y = H - (Math.max(0, Math.min(100, v)) / 100) * H;
    d += `${pen ? 'L' : 'M'}${x.toFixed(1)} ${y.toFixed(1)}`;
    pen = true;
  }
  return d;
}

function summary(samples: readonly Telemetry[]): string {
  const mins = Math.max(1, Math.round((samples[samples.length - 1]!.at_ms - samples[0]!.at_ms) / 60_000));
  const parts = SERIES.map(({ label, pick }) => {
    const vs = samples.map(pick).filter((v): v is number => v !== null);
    return vs.length === 0 ? `${label} n/a` : `${label} now ${Math.round(vs[vs.length - 1]!)}%, peak ${Math.round(Math.max(...vs))}%`;
  });
  return `Last ${mins} min: ${parts.join('; ')}`;
}

function StatsChart({ samples }: { samples: readonly Telemetry[] }) {
  if (samples.length < 2) return <p class="muted small">Collecting history...</p>;
  return (
    <div class="stack tight">
      <svg class="chart" viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-label={summary(samples)}>
        {[25, 50, 75].map((p) => <line key={p} class="chart-grid" x1={0} x2={W} y1={H - (p / 100) * H} y2={H - (p / 100) * H} vector-effect="non-scaling-stroke" />)}
        {SERIES.map(({ key, pick }) => <path key={key} class={`chart-line ${key}`} d={linePath(samples, pick)} vector-effect="non-scaling-stroke" />)}
      </svg>
      <div class="row small muted">
        <span>{clock(samples[0]!.at_ms)}</span>
        <ul class="chart-legend grow">{SERIES.map(({ key, label }) => <li key={key}><span class={`swatch ${key}`} aria-hidden="true" />{label}</li>)}</ul>
        <span>{clock(samples[samples.length - 1]!.at_ms)}</span>
      </div>
    </div>
  );
}

// ---- repos and cleanup ----

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

/** A node's repo index, cleanup buttons and the last clean report. */
function NodeRepos({ n }: { n: NodeInfo }) {
  const s = store.state.value;
  const repos = s.nodeRepos[n.id] ?? [];
  const report = s.lastClean[n.id];
  const off = n.state === 'Offline';
  return (
    <section class="card stack" aria-label={`Repositories on ${n.hostname}`}>
      <div class="card-title"><h2>Repositories</h2></div>
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

// ---- hub-stored settings ----

const TRI = [{ value: 'inherit', label: 'Node default' }, { value: 'on', label: 'On' }, { value: 'off', label: 'Off' }] as const;

function TriField({ label, value, onChange }: { label: string; value: Tri; onChange(v: Tri): void }) {
  return <div class="field"><span class="label">{label}</span><Segmented<Tri> label={label} value={value} options={TRI} onChange={onChange} /></div>;
}

/** Mount with `key` = node id (plus whether settings exist): edits survive unrelated config broadcasts. */
function SettingsCard({ node, stored }: { node: string; stored: NodeSettings | undefined }) {
  const [form, setForm] = useState<SettingsForm>(() => settingsToForm(stored));
  const rootsId = useId();
  const set = <K extends keyof SettingsForm>(k: K) => (v: SettingsForm[K]) => setForm((f) => ({ ...f, [k]: v }));
  const e = settingsErrors(form);
  const hint = 'Empty: the node decides.';
  return (
    <Card title="Settings">
      <form class="stack" aria-label="Node settings" onSubmit={(ev) => { ev.preventDefault(); store.intent({ kind: 'save_node_settings', node, form }); }}>
        <p class="muted small">Stored on the hub and applied live; they override the node's own files.</p>
        <div class="field">
          <label for={rootsId}>Repository folders</label>
          <textarea id={rootsId} class="input mono" rows={3} placeholder="~/code" spellcheck={false} value={form.repoRoots} aria-invalid={e.repoRoots ? true : undefined} onInput={(ev) => set('repoRoots')(ev.currentTarget.value)} />
          {e.repoRoots ? <span class="err" role="alert">{e.repoRoots}</span> : <span class="hint">One per line, searched for existing clones. {hint}</span>}
        </div>
        <div class="grid">
          <TextField label="Index every (minutes)" value={form.indexIntervalMins} placeholder="node default" error={e.indexIntervalMins} hint="1-1440" onValue={set('indexIntervalMins')} />
          <TextField label="Clean after exit (hours)" value={form.cleanAfterExitHours} placeholder="node default" error={e.cleanAfterExitHours} hint="1-8760" onValue={set('cleanAfterExitHours')} />
          <TextField label="Busy above CPU %" value={form.cpuBusyPct} placeholder="node default" error={e.cpuBusyPct} hint="1-100" onValue={set('cpuBusyPct')} />
          <TextField label="Busy above GPU %" value={form.gpuBusyPct} placeholder="node default" error={e.gpuBusyPct} hint="1-100" onValue={set('gpuBusyPct')} />
        </div>
        <TriField label="Clean up automatically" value={form.cleanAuto} onChange={set('cleanAuto')} />
        <TriField label="Install updates automatically" value={form.autoUpdate} onChange={set('autoUpdate')} />
        <div class="row">
          <Button type="submit" variant="primary" size="sm" disabled={Object.keys(e).length > 0}>Save settings</Button>
          <ConfirmButton variant="ghost" label="Reset to node defaults" confirmLabel="Clears the hub settings: confirm" disabled={!stored} onConfirm={() => store.intent({ kind: 'save_node_settings', node, form: settingsToForm() })} />
        </div>
      </form>
    </Card>
  );
}

// ---- the screen ----

export function NodeDetail({ id }: { id: string }) {
  const s = store.state.value;
  const n = s.nodes.find((x) => x.id === id);
  const off = !n || n.state === 'Offline';
  // Ask for the history on open and whenever the node comes back; stop collecting on leave.
  useEffect(() => {
    store.intent({ kind: 'watch_node', node: id });
    return () => { store.intent({ kind: 'unwatch_node', node: id }); };
  }, [id, off]);
  const back = <a href={href('nodes')}>All nodes</a>;
  if (!n) return <EmptyState title="Unknown node" action={back}>No node with id {id} has reported to the hub.</EmptyState>;

  const v = s.nodeVersions[id];
  const u = s.updates[id];
  const upd = updateView(u);
  const working = u !== undefined && !['up_to_date', 'available', 'failed'].includes(u.state);
  const stats = s.stats?.node === id ? s.stats : null;
  const t = n.telemetry;
  const reported = s.nodeProfiles[id] ?? [];
  const failures = Object.entries(s.lastFailure).filter(([, f]) => f.node === id);
  const stored = s.nodeSettings[id];
  return (
    <>
      <nav class="small">{back}</nav>
      <header class="page-head">
        <h1>{n.hostname}</h1>
        <Pill tone={nodeStateTone(n.state)}>{nodeStateLabel(n.state)}</Pill>
      </header>
      <section class="card stack tight" aria-label="Status">
        <div class="muted small">
          {v ? `${v.os} ${v.arch} · ulak ${v.version} (${v.git.slice(0, 7)})` : 'unknown platform'}
          {' · '}
          {stats?.connectedMs ? `connected since ${clock(stats.connectedMs)}, ${day(stats.connectedMs)}` : off ? 'offline' : 'connected'}
        </div>
        <div class="row">
          <span class={`upd ${upd.tone}`}>{upd.label}</span>
          <Button size="sm" variant="ghost" disabled={off || working} onClick={() => store.intent({ kind: 'check_update', node: id })}>Check</Button>
          {availableVersion(u) && <ConfirmButton variant="primary" label="Update" confirmLabel="Restarts the node: confirm" disabled={off} onConfirm={() => store.intent({ kind: 'update_node', node: id })} />}
        </div>
      </section>

      <Card title="Load">
        <div class="stack tight">
          <ProgressBar label="CPU" value={off ? null : t.cpu_pct} />
          <ProgressBar label="GPU" value={off ? null : t.gpu_pct} />
          <ProgressBar label="Mem" value={off ? null : t.mem_used_pct} />
          <span class="muted small">Load average (1 min): {off ? 'n/a' : t.load1.toFixed(2)}</span>
        </div>
        <StatsChart samples={stats?.samples ?? []} />
      </Card>

      <Card title="Agents">
        {n.agents.length === 0 && failures.length === 0 && <p class="muted small">No agents on this node.</p>}
        {n.agents.length > 0 && (
          <ul class="agent-list" aria-label="Agents on this node">
            {n.agents.map((a) => <li key={a} class="row"><span class="strong grow">{a}</span><Pill tone={agentTone(s.statuses[a])}>{agentStatusLabel(s.statuses[a])}</Pill></li>)}
          </ul>
        )}
        {failures.length > 0 && (
          <ul class="repo-list small" aria-label="Recent failures">
            {failures.map(([a, f]) => <li key={a}><span class="strong">{a}</span><span class="err">{f.reason || 'failed'}</span></li>)}
          </ul>
        )}
      </Card>

      <Card title="Profiles">
        {reported.length === 0 ? <p class="muted small">The node has not reported any profiles.</p> : (
          <ul class="repo-list small" aria-label={`Profiles on ${n.hostname}`}>
            {reported.map((p) => (
              <li key={p.name}>
                <span class="strong">{p.name}</span>
                <span class="muted mono">{p.config_dir ?? 'default config'}</span>
                <Pill tone={p.installed ? 'ok' : 'muted'}>{p.installed ? 'Installed' : 'Not installed'}</Pill>
                {p.login_blocked && <Pill tone="warn">Login required</Pill>}
                {p.from_hub && <Pill tone="info">from hub</Pill>}
              </li>
            ))}
          </ul>
        )}
        <strong class="small">Hub profiles for this node only</strong>
        <HubProfiles node={id} label={`Hub profiles of ${n.hostname}`} />
        <p class="muted small">Global profiles are on the <a href={href('settings')}>Settings</a> page.</p>
      </Card>

      <NodeRepos n={n} />
      <SettingsCard key={`${id}:${stored ? 1 : 0}`} node={id} stored={stored} />
    </>
  );
}
