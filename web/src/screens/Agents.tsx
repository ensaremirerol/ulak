import { useState } from 'preact/hooks';
import type { NodeInfo, QueuedSpawn } from '../proto';
import { agentStatusLabel, agentTone, awaitsSession, availableVersion, kindTitle, keysFromText, nodeOf, store } from '../state';
import { Button, Card, Dot, EmptyState, Pill, ProgressBar } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { ageLabel, nodeStateLabel, nodeStateTone } from '../ui/format';
import { Terminal } from '../ui/Terminal';
import { SpawnDialog } from './SpawnDialog';

/** Escape sequences for the phone key bar. */
export const KEYS: readonly (readonly [label: string, seq: string])[] = [
  ['Esc', '\x1b'], ['Tab', '\t'], ['Ctrl+C', '\x03'], ['Up', '\x1b[A'], ['Down', '\x1b[B'], ['Left', '\x1b[D'], ['Right', '\x1b[C'], ['Enter', '\r'], ['y', 'y'], ['n', 'n'],
];

function NodeCard({ n }: { n: NodeInfo }) {
  const s = store.state.value;
  const t = n.telemetry;
  const off = n.state === 'Offline';
  const blockers = s.blockers.filter((b) => b.node === n.id);
  const update = availableVersion(s.updates[n.id]);
  const note = typeof n.state === 'object' ? 'Not taking new agents' : off ? 'Offline' : '';
  return (
    <article class="card node-card" aria-label={`Node ${n.hostname}`}>
      <div class="card-title"><strong>{n.hostname}</strong><Pill tone={nodeStateTone(n.state)} title={nodeStateLabel(n.state)}>{nodeStateLabel(n.state)}</Pill></div>
      <div class="stack tight">
        <ProgressBar label="CPU" value={off ? null : t.cpu_pct} />
        <ProgressBar label="GPU" value={off ? null : t.gpu_pct} />
      </div>
      <div class="row muted small">
        <span>{n.agents.length === 1 ? '1 agent' : `${n.agents.length} agents`}</span>
        {note && <span>{note}</span>}
      </div>
      {(blockers.length > 0 || update) && (
        <div class="row">
          {blockers.map((b) => <Pill key={b.id} tone="warn">{kindTitle(b.kind)}: {b.profile.split('@')[0]}</Pill>)}
          {update && <Pill tone="info">Update {update}</Pill>}
        </div>
      )}
    </article>
  );
}

function AgentTerminal({ agent }: { agent: string }) {
  const s = store.state.value;
  const [text, setText] = useState('');
  const node = nodeOf(s, agent);
  const st = s.statuses[agent];
  const send = (seq: string) => store.intent({ kind: 'send_keys', data: keysFromText(seq) });
  return (
    <div class="term-card" role="region" aria-label={`Terminal of ${agent}`}>
      <div class="term-head">
        <div class="row"><Dot tone={agentTone(st)} /><strong>{agent}</strong>{node && <span class="muted">on {node.hostname}</span>}<Pill tone={agentTone(st)}>{agentStatusLabel(st)}</Pill></div>
        <div class="row">
          <Button size="sm" variant="ghost" onClick={() => store.intent({ kind: 'detach' })}>Detach</Button>
          <ConfirmButton label="Kill" confirmLabel="Click again to kill" onConfirm={() => store.intent({ kind: 'kill', agent })} />
        </div>
      </div>
      {awaitsSession(st)
        ? <div class="term-host term-wait muted" role="status">{st === 'Installing' ? 'Installing…' : 'Starting…'} The terminal opens when the agent is running (a repository is cloned first).</div>
        : <Terminal agent={agent} />}
      <div class="keybar" aria-label="Keys">
        {KEYS.map(([label, seq]) => <button key={label} type="button" class="key" onClick={() => send(seq)}>{label}</button>)}
      </div>
      <form
        class="term-send"
        onSubmit={(e) => {
          e.preventDefault();
          if (text && send(text + '\r')) setText('');
        }}
      >
        <label class="sr-only" for="term-msg">Send input to {agent}</label>
        <input id="term-msg" class="input mono" type="text" autocomplete="off" placeholder="Send input to the agent" value={text} onInput={(e) => setText(e.currentTarget.value)} />
        <Button type="submit" variant="primary" aria-label="Send">Send</Button>
      </form>
    </div>
  );
}

const sourceLabel = (q: QueuedSpawn): string => (q.source.kind === 'repo' ? `${q.source.repo}@${q.source.branch ?? 'default'}` : q.source.path);
const targetLabel = (q: QueuedSpawn): string => (q.target === 'Auto' ? 'Auto' : `node ${q.target.Node}`);

function QueuePanel({ items, now }: { items: QueuedSpawn[]; now: number }) {
  return (
    <Card dashed title="Queue" aside={<span class="muted small">{items.length} waiting</span>}>
      <ul class="agent-list" aria-label="Queue">
        {items.map((q) => (
          <li key={q.id} class="row">
            <span class="agent-name">
              <span>{q.agent}</span>
              <span class="muted small">{sourceLabel(q)} · {targetLabel(q)} · {ageLabel(Math.max(0, now - q.created_ms))}</span>
              {typeof q.state === 'object' && <span class="err small" role="alert">{q.state.Failed}</span>}
            </span>
            <Pill tone={q.state === 'Queued' ? 'info' : 'bad'}>{q.state === 'Queued' ? 'Queued' : 'Failed'}</Pill>
            <ConfirmButton label="Cancel" confirmLabel="Click again to cancel" onConfirm={() => store.intent({ kind: 'cancel_queued', id: q.id })} />
          </li>
        ))}
      </ul>
    </Card>
  );
}

export function Agents() {
  const s = store.state.value;
  const [spawnOpen, setSpawnOpen] = useState(false);
  const agents = s.nodes.flatMap((n) => n.agents.map((id) => ({ id, node: n })));
  const asking = new Set(s.questions.map((q) => q.from));
  return (
    <>
      <header class="page-head">
        <h1>Agents</h1>
        <Button class="hide-phone" variant="primary" onClick={() => setSpawnOpen(true)}>New agent</Button>
      </header>
      <section class={`console${s.selected ? ' has-sel' : ''}`}>
        <div class="node-grid" aria-label="Nodes">
          {s.nodes.length === 0 && <EmptyState title="No nodes connected">Enrol a node from the Devices page.</EmptyState>}
          {s.nodes.map((n) => <NodeCard key={n.id} n={n} />)}
        </div>
        <div class="console-body">
          <div class="agent-col">
            {agents.length === 0 ? (
              <EmptyState title="No agents yet">Spawn one to get started.</EmptyState>
            ) : (
              <Card>
                <ul class="agent-list" aria-label="Agents">
                  {agents.map(({ id, node }) => {
                    const st = s.statuses[id];
                    const label = asking.has(id) ? 'Asking' : agentStatusLabel(st);
                    const tone = asking.has(id) ? 'info' : agentTone(st);
                    return (
                      <li key={id}>
                        <button type="button" class="agent-row" aria-current={s.selected === id ? 'true' : undefined} onClick={() => store.intent({ kind: 'select', agent: id })}>
                          <Dot tone={tone} />
                          <span class="agent-name"><span>{id}</span><span class="muted small">on {node.hostname}</span></span>
                          <span class={`status ${tone}`}>{label}</span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </Card>
            )}
            {s.queue.length > 0 && <QueuePanel items={s.queue} now={s.now} />}
          </div>
          {s.selected ? <AgentTerminal agent={s.selected} /> : agents.length > 0 && <div class="term-card empty-term muted">Select an agent to open its terminal.</div>}
        </div>
      </section>
      <Button class="fab show-phone" variant="primary" aria-label="New agent" onClick={() => setSpawnOpen(true)}>+</Button>
      {spawnOpen && <SpawnDialog onClose={() => setSpawnOpen(false)} />}
    </>
  );
}
