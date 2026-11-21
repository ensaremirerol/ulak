import { signal } from '@preact/signals';
import { useEffect, useRef, useState } from 'preact/hooks';
import type { AgentStatus, NodeInfo, QueuedSpawn } from '../proto';
import { agentStatusLabel, agentTone, awaitsSession, keysFromText, nodeOf, store, streams, versionAtLeast, type State } from '../state';
import { Button, Dot, EmptyState, Pill, type Tone } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { ageLabel } from '../ui/format';
import { nodeHref, termHref } from '../ui/router';
import { Terminal, type TerminalHandle, type TermMode } from '../ui/Terminal';
import { SpawnDialog } from './SpawnDialog';

/** Escape sequences for the phone key bar. */
export const KEYS: readonly (readonly [label: string, seq: string])[] = [
  ['Esc', '\x1b'], ['Tab', '\t'], ['Ctrl+C', '\x03'], ['Up', '\x1b[A'], ['Down', '\x1b[B'], ['Left', '\x1b[D'], ['Right', '\x1b[C'], ['Enter', '\r'], ['y', 'y'], ['n', 'n'],
];

/** Nodes from this version resize the agent's pane to the viewer (`resize` frame); older ones keep 200x50. */
export const RESIZE_SINCE = [0, 1, 10] as const;
export const supportsResize = (version: string | undefined): boolean => versionAtLeast(version, RESIZE_SINCE);

export interface AgentRow { id: string; node: NodeInfo | undefined; status: AgentStatus | undefined }
export interface NeedsRow extends AgentRow { kind: string; reason: string; action: 'answer' | 'inbox' | 'open' | 'remove' }

/** The agent list grouped by what it needs from the user. */
export function groupAgents(s: State, filter = ''): { needs: NeedsRow[]; running: AgentRow[]; ended: AgentRow[] } {
  const ids = new Set<string>(s.nodes.flatMap((n) => n.agents));
  for (const id of Object.keys(s.lastFailure)) ids.add(id);
  for (const id of Object.keys(s.statuses)) ids.add(id);
  for (const b of s.blockers) if (b.agent) ids.add(b.agent);
  for (const q of s.questions) ids.add(q.from);
  const f = filter.trim().toLowerCase();
  const needs: NeedsRow[] = [], running: AgentRow[] = [], ended: AgentRow[] = [];
  for (const id of [...ids].sort()) {
    const status = s.statuses[id];
    const node = nodeOf(s, id) ?? s.nodes.find((n) => n.id === s.lastFailure[id]?.node);
    if (f && !id.toLowerCase().includes(f) && !(node?.hostname.toLowerCase().includes(f) ?? false)) continue;
    const row = { id, node, status };
    const q = s.questions.find((x) => x.from === id);
    const b = s.blockers.find((x) => x.agent === id);
    if (q) needs.push({ ...row, kind: 'Asks you', reason: q.text, action: 'answer' });
    else if (b) needs.push({ ...row, kind: b.kind === 'Install' ? 'Install failed' : 'Log in', reason: b.detail, action: 'inbox' });
    else if (status === 'NeedsLogin') needs.push({ ...row, kind: 'Log in', reason: 'The agent asked you to log in: open its terminal.', action: 'open' });
    else if (status === 'Failed' || (status === undefined && s.lastFailure[id])) needs.push({ ...row, kind: 'Failed', reason: s.lastFailure[id]?.reason ?? 'It stopped with an error.', action: 'remove' });
    else if (typeof status === 'object') ended.push(row);
    else if (row.node?.agents.includes(id) || awaitsSession(status) || status === 'Running') running.push(row);
  }
  return { needs, running, ended };
}

/** The agent shown in a separate window, if any (this tab then shows a placeholder). */
const poppedOut = signal<{ agent: string; win: Window } | null>(null);

function popOut(agent: string) {
  const win = window.open(`${location.pathname}${location.search}${termHref(agent)}`, `ulak-term-${agent}`, 'popup,width=1100,height=720');
  if (!win) {
    store.toast('info', 'The browser blocked the new window: allow pop-ups for this site.');
    return;
  }
  poppedOut.value = { agent, win };
  store.intent({ kind: 'detach' });
}

function bringBack(agent: string) {
  const p = poppedOut.value;
  poppedOut.value = null;
  if (p && !p.win.closed) p.win.close();
  store.intent({ kind: 'select', agent });
}

/** Status pill text and tone, with Asking for an open question. */
function statusOf(s: State, id: string): [string, Tone] {
  if (s.questions.some((q) => q.from === id)) return ['Asking', 'info'];
  const st = s.statuses[id];
  return [agentStatusLabel(st), agentTone(st)];
}

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    store.toast('ok', 'Copied.');
  } catch {
    store.toast('info', 'Copy is not available here: select the text and press Ctrl+C.');
  }
}

type Tab = 'terminal' | 'messages' | 'details';

/** One agent: header with actions, then Terminal / Messages / Details. `popout`: the separate-window view. */
export function AgentPanel({ agent, popout = false }: { agent: string; popout?: boolean }) {
  const s = store.state.value;
  const [tab, setTab] = useState<Tab>('terminal');
  const [text, setText] = useState('');
  const [full, setFull] = useState(false);
  const card = useRef<HTMLElement>(null);
  const term = useRef<TerminalHandle>(null);
  const node = nodeOf(s, agent) ?? s.nodes.find((n) => n.id === s.lastFailure[agent]?.node);
  const st = s.statuses[agent];
  const [label, tone] = statusOf(s, agent);
  const mode: TermMode = streams(s, agent) ? 'stream' : supportsResize(node ? s.nodeVersions[node.id]?.version : undefined) ? 'fit' : 'fixed';
  const send = (seq: string) => store.intent({ kind: 'send_keys', data: keysFromText(seq) });

  useEffect(() => {
    const on = () => setFull(document.fullscreenElement !== null && document.fullscreenElement === card.current);
    document.addEventListener('fullscreenchange', on);
    return () => document.removeEventListener('fullscreenchange', on);
  }, []);
  useEffect(() => {
    if (tab === 'messages' && s.conn === 'open') store.intent({ kind: 'refresh_mailbox' });
  }, [tab, s.conn]);

  const fullscreen = async () => {
    try {
      if (document.fullscreenElement) {
        await document.exitFullscreen();
        return;
      }
      await card.current?.requestFullscreen();
      // Esc belongs to the agent: with a keyboard lock the browser exits only on a long Esc press.
      await (navigator as Navigator & { keyboard?: { lock?: (keys: string[]) => Promise<void> } }).keyboard?.lock?.(['Escape']);
    } catch {
      /* no fullscreen or no keyboard lock in this browser */
    }
  };

  const messages = s.mailbox.filter((m) => m.from === agent || m.to === agent).sort((a, b) => b.at_ms - a.at_ms);
  return (
    <section ref={card} class={`agent-panel${popout ? ' popout' : ''}`} role="region" aria-label={`Terminal of ${agent}`}>
      <div class="panel-head">
        <div class="panel-title">
          <div class="row nowrap"><Dot tone={tone} /><strong class="mono">{agent}</strong><Pill tone={tone}>{label}</Pill></div>
          <span class="muted small">{node ? `on ${node.hostname}` : 'node unknown'}</span>
        </div>
        <div class="row">
          {!awaitsSession(st) && <Button size="sm" variant="ghost" title="Copies the selection, or the whole screen" onClick={() => void copyText(term.current?.text() ?? '')}>Copy</Button>}
          <Button size="sm" variant="ghost" aria-pressed={full} onClick={() => void fullscreen()}>{full ? 'Exit full screen' : 'Full screen'}</Button>
          {!popout && <Button size="sm" variant="ghost" class="hide-phone" onClick={() => popOut(agent)}>New window</Button>}
          {!popout && <Button size="sm" variant="ghost" onClick={() => store.intent({ kind: 'detach' })}>Close</Button>}
          <ConfirmButton label="Stop agent" confirmLabel="Click again to stop" onConfirm={() => store.intent({ kind: 'kill', agent })} />
        </div>
      </div>
      <div class="tabs" role="tablist" aria-label="Agent views">
        {(['terminal', 'messages', 'details'] as const).map((t) => (
          <button key={t} type="button" role="tab" class="tab" aria-selected={tab === t} onClick={() => setTab(t)}>
            {t === 'terminal' ? 'Terminal' : t === 'messages' ? `Messages${messages.length ? ` ${messages.length}` : ''}` : 'Details'}
          </button>
        ))}
      </div>
      {tab === 'terminal' && (
        <>
          {awaitsSession(st)
            ? <div class="term-host term-wait muted" role="status">{st === 'Installing' ? 'Installing…' : 'Starting…'} The terminal opens when the agent is running (a repository is cloned first).</div>
            : <Terminal agent={agent} mode={mode} handle={term} />}
          <div class="keybar" aria-label="Keys">
            {KEYS.map(([k, seq]) => <button key={k} type="button" class="key" onClick={() => send(seq)}>{k}</button>)}
          </div>
          <form class="term-send" onSubmit={(e) => { e.preventDefault(); if (text && send(text + '\r')) setText(''); }}>
            <label class="sr-only" for={`term-msg-${agent}`}>Send input to {agent}</label>
            <input id={`term-msg-${agent}`} class="input mono" type="text" autocomplete="off" placeholder={`Message ${agent} (Enter sends; or type in the terminal)`} value={text} onInput={(e) => setText(e.currentTarget.value)} />
            <Button type="submit" variant="primary" aria-label="Send">Send</Button>
          </form>
        </>
      )}
      {tab === 'messages' && (
        <ul class="panel-body plain" aria-label={`Messages of ${agent}`}>
          {messages.length === 0 && <li class="muted">No messages to or from {agent}.</li>}
          {messages.map((m) => (
            <li key={m.id} class="msg">
              <span class="muted small">{m.from} {'->'} {m.to} · {ageLabel(Math.max(0, s.now - m.at_ms))} · {m.status}</span>
              <span class="pre-wrap">{m.payload}</span>
            </li>
          ))}
        </ul>
      )}
      {tab === 'details' && (
        <dl class="panel-body details">
          <dt>Node</dt><dd>{node ? <a href={nodeHref(node.id)}>{node.hostname}</a> : 'unknown'}</dd>
          <dt>Status</dt><dd>{label}</dd>
          {s.lastFailure[agent] && <><dt>Last failure</dt><dd class="pre-wrap">{s.lastFailure[agent].reason}</dd></>}
          <dt>Terminal</dt><dd>{mode === 'stream' ? 'Live stream, sized to this window. Drag to copy, Shift+drag to select here, wheel to scroll back.' : mode === 'fit' ? 'Sized to this window (update the node to 0.1.11 for the live stream)' : 'Fixed 200 x 50 (update the node to 0.1.11 for the live stream)'}</dd>
        </dl>
      )}
    </section>
  );
}

const sourceLabel = (q: QueuedSpawn): string => (q.source.kind === 'repo' ? `${q.source.repo}@${q.source.branch ?? 'default'}` : q.source.path);
const targetLabel = (q: QueuedSpawn): string => (q.target === 'Auto' ? 'Auto' : `node ${q.target.Node}`);

function NeedsCard({ r }: { r: NeedsRow }) {
  const open = () => store.intent({ kind: 'select', agent: r.id });
  return (
    <li class="needs-card" aria-label={`${r.id}: ${r.kind}`}>
      <div class="row nowrap"><strong class="mono grow">{r.id}</strong><span class="small needs-kind">{r.kind}</span></div>
      {r.node && <span class="small">on {r.node.hostname}</span>}
      <span class="small pre-wrap">{r.reason}</span>
      <div class="row">
        {r.action === 'answer' && <a class="btn sm warn-btn" href="#/inbox">Answer</a>}
        {r.action === 'inbox' && <a class="btn sm warn-btn" href="#/inbox">Fix in Inbox</a>}
        {r.action === 'open' && <Button size="sm" class="warn-btn" onClick={open}>Open terminal</Button>}
        {r.action === 'remove' && <ConfirmButton label="Remove" confirmLabel="Click again to remove" onConfirm={() => store.intent({ kind: 'kill', agent: r.id })} />}
        {r.action !== 'open' && <Button size="sm" variant="ghost" onClick={open}>Open</Button>}
      </div>
    </li>
  );
}

function Row({ r, selected }: { r: AgentRow; selected: boolean }) {
  const s = store.state.value;
  const [label, tone] = statusOf(s, r.id);
  return (
    <li>
      <button type="button" class="agent-row" aria-current={selected ? 'true' : undefined} onClick={() => store.intent({ kind: 'select', agent: r.id })}>
        <Dot tone={tone} />
        <span class="agent-name"><span class="mono">{r.id}</span><span class="muted small">{r.node ? `on ${r.node.hostname}` : 'node unknown'}</span></span>
        <span class={`status ${tone}`}>{label}</span>
      </button>
    </li>
  );
}

export function Agents() {
  const s = store.state.value;
  const [spawnOpen, setSpawnOpen] = useState(false);
  const [filter, setFilter] = useState('');
  const [showEnded, setShowEnded] = useState(false);
  const { needs, running, ended } = groupAgents(s, filter);
  const popped = poppedOut.value;

  // The separate window was closed: show the agent here again.
  useEffect(() => {
    if (!popped) return;
    const t = setInterval(() => {
      if (popped.win.closed && poppedOut.value === popped) bringBack(popped.agent);
    }, 1000);
    return () => clearInterval(t);
  }, [popped]);

  const empty = needs.length + running.length + ended.length === 0;
  return (
    <>
      <header class="page-head">
        <h1>Agents</h1>
        <div class="row">
          <label class="sr-only" for="agent-filter">Filter agents</label>
          <input id="agent-filter" class="input filter" type="search" placeholder="Filter by name or node" value={filter} onInput={(e) => setFilter(e.currentTarget.value)} />
          <Button class="hide-phone" variant="primary" onClick={() => setSpawnOpen(true)}>New agent</Button>
        </div>
      </header>
      <section class={`console${s.selected ? ' has-sel' : ''}`}>
        {s.nodes.length === 0 && <EmptyState title="No nodes connected">Add one on the <a href="#/nodes">Nodes</a> page.</EmptyState>}
        <div class="console-body">
          <div class="agent-col">
            {empty && s.queue.length === 0 && <EmptyState title={filter ? 'No matching agents' : 'No agents yet'}>{filter ? 'Clear the filter to see all agents.' : 'Start one with New agent.'}</EmptyState>}
            {needs.length > 0 && (
              <section aria-label="Needs you" class="group">
                <h2 class="group-title warn">Needs you · {needs.length}</h2>
                <ul class="plain">{needs.map((r) => <NeedsCard key={r.id} r={r} />)}</ul>
              </section>
            )}
            {running.length > 0 && (
              <section aria-label="Running" class="group">
                <h2 class="group-title">Running · {running.length}</h2>
                <ul class="agent-list" aria-label="Agents">{running.map((r) => <Row key={r.id} r={r} selected={s.selected === r.id} />)}</ul>
              </section>
            )}
            {s.queue.length > 0 && (
              <section aria-label="Waiting for a node" class="group">
                <h2 class="group-title">Waiting for a node · {s.queue.length}</h2>
                <ul class="plain" aria-label="Queue">
                  {s.queue.map((q) => (
                    <li key={q.id} class="queue-row">
                      <span class="agent-name">
                        <span class="mono">{q.agent}</span>
                        <span class="muted small">{sourceLabel(q)} · {targetLabel(q)} · {ageLabel(Math.max(0, s.now - q.created_ms))}</span>
                        {typeof q.state === 'object' ? <span class="err small" role="alert">{q.state.Failed}</span> : <span class="muted small">Starts when a node is free.</span>}
                      </span>
                      <ConfirmButton label="Cancel" confirmLabel="Click again to cancel" onConfirm={() => store.intent({ kind: 'cancel_queued', id: q.id })} />
                    </li>
                  ))}
                </ul>
              </section>
            )}
            {ended.length > 0 && (
              <section aria-label="Ended" class="group">
                <button type="button" class="linkish muted" aria-expanded={showEnded} onClick={() => setShowEnded(!showEnded)}>{showEnded ? 'Hide' : 'Show'} {ended.length} ended</button>
                {showEnded && <ul class="agent-list" aria-label="Ended agents">{ended.map((r) => <Row key={r.id} r={r} selected={s.selected === r.id} />)}</ul>}
              </section>
            )}
          </div>
          {popped && !s.selected
            ? (
              <div class="agent-panel empty-term">
                <p><strong class="mono">{popped.agent}</strong> is open in a separate window.</p>
                <Button onClick={() => bringBack(popped.agent)}>Show it here</Button>
              </div>
            )
            : s.selected
              ? <AgentPanel key={s.selected} agent={s.selected} />
              : !empty && <div class="agent-panel empty-term muted">Select an agent to open its terminal.</div>}
        </div>
      </section>
      {!s.selected && <Button class="fab show-phone" variant="primary" aria-label="New agent" onClick={() => setSpawnOpen(true)}>+</Button>}
      {spawnOpen && <SpawnDialog onClose={() => setSpawnOpen(false)} />}
    </>
  );
}

/** The separate-window view (`#/term/<agent>`): just the agent, attached once connected. */
export function AgentWindow({ agent }: { agent: string }) {
  const s = store.state.value;
  useEffect(() => {
    if (s.conn === 'open' && store.state.peek().selected !== agent) store.intent({ kind: 'select', agent });
  }, [s.conn, agent]);
  useEffect(() => {
    document.title = `${agent} · ulak`;
  }, [agent]);
  return <main class="agent-window"><AgentPanel agent={agent} popout /></main>;
}
