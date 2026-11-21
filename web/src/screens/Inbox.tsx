import { useEffect, useState } from 'preact/hooks';
import type { Blocker, Forward, Issue, Question } from '../proto';
import { installKey, kindTitle, questionAgeMs, store, waitingForwards, type State } from '../state';
import { Button, EmptyState } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { ageLabel } from '../ui/format';
import { navigate, nodeHref } from '../ui/router';
import { IssueAgent } from './IssueAgent';

function openAgent(agent: string) {
  const s = store.state.peek();
  if (s.selected === agent || store.intent({ kind: 'select', agent })) navigate('agents');
}

function Card({ kind, tone, title, age, label, children }: { kind: string; tone: 'warn' | 'info' | 'bad'; title: preact.ComponentChildren; age: number; label: string; children: preact.ComponentChildren }) {
  return (
    <article class={`inbox-card ${tone}`} aria-label={label}>
      <div class="row nowrap">
        <span class={`inbox-kind ${tone}`}>{kind}</span>
        <strong class="grow">{title}</strong>
        <span class="muted small">{ageLabel(age)}</span>
      </div>
      {children}
    </article>
  );
}

function QuestionCard({ q }: { q: Question }) {
  const [text, setText] = useState('');
  const age = questionAgeMs(store.state.value, q);
  const send = () => {
    store.intent({ kind: 'answer', question: q.id, text });
  };
  return (
    <Card kind="Question" tone="info" age={age} label={`Question from ${q.from}`} title={<>{q.from} <button type="button" class="linkish small" onClick={() => openAgent(q.from)}>Open terminal</button></>}>
      <span class="question-text">{q.text}</span>
      <form class="row nowrap" onSubmit={(e) => { e.preventDefault(); send(); }}>
        <label class="sr-only" for={`ans-${q.id}`}>Answer to {q.from}</label>
        <input id={`ans-${q.id}`} class="input grow" type="text" autocomplete="off" placeholder="Your answer" value={text} onInput={(e) => setText(e.currentTarget.value)} />
        <Button type="submit" variant="primary">Send</Button>
      </form>
    </Card>
  );
}

/** Agents on the blocker's node that wait for it (installing or waiting for a login). */
function pausedAgents(s: State, b: Blocker): string[] {
  const node = s.nodes.find((n) => n.id === b.node);
  const waiting = b.kind === 'Install' ? 'Installing' : 'NeedsLogin';
  const ids = new Set([...(node?.agents ?? []), ...(b.agent ? [b.agent] : [])]);
  return [...ids].filter((a) => s.statuses[a] === waiting || (a === b.agent && s.statuses[a] !== 'Running' && typeof s.statuses[a] !== 'object'));
}

/** The node profile a blocker's key (`binary@config`) names: install retries need its name. */
function profileOf(s: State, b: Blocker): string {
  const [bin, dir] = b.profile.split('@') as [string, string | undefined];
  const p = s.nodeProfiles[b.node]?.find((x) => x.binary.split('/').pop() === bin && (x.config_dir ?? undefined) === dir);
  return p?.name ?? bin;
}

function BlockerCard({ b }: { b: Blocker }) {
  const s = store.state.value;
  const host = s.nodes.find((n) => n.id === b.node)?.hostname ?? b.node;
  const name = b.profile.split('@')[0];
  const paused = pausedAgents(s, b);
  const profile = profileOf(s, b);
  const run = s.installs[installKey(b.node, profile)];
  const pausedLine = paused.length > 0 && <span class="small">Agents for <span class="mono">{name}</span> on this node are paused: {paused.join(', ')}.</span>;
  if (b.kind === 'Login') {
    return (
      <Card kind="Log in" tone="warn" age={s.now - b.at_ms} label={`${kindTitle(b.kind)} on ${host}`} title={`${b.profile} on ${host} is not logged in`}>
        <span class="small">Open the agent's terminal, run <span class="mono">/login</span> and follow the link. New agents on this profile wait until then.</span>
        {pausedLine}
        {b.detail && <details><summary class="small">What the agent said</summary><pre class="mono detail">{b.detail}</pre></details>}
        <div class="row">
          {b.agent && <Button variant="primary" onClick={() => openAgent(b.agent!)}>Open terminal to log in</Button>}
          <Button variant="ghost" onClick={() => store.intent({ kind: 'resolve_blocker', id: b.id })}>I logged in, retry</Button>
        </div>
      </Card>
    );
  }
  return (
    <Card kind="Install failed" tone="bad" age={s.now - b.at_ms} label={`${kindTitle(b.kind)} on ${host}`} title={`${name} could not be installed on ${host}`}>
      {pausedLine}
      {b.detail && <details><summary class="small">Show the installer output</summary><pre class="mono detail">{b.detail}</pre></details>}
      {run?.running && <p class="muted small" role="status">Installing again...</p>}
      {run && !run.running && !run.ok && run.detail && <p class="err small" role="alert">The retry failed too: {run.detail.slice(-300)}</p>}
      {run && !run.running && run.ok && <p class="small ok-note" role="status">Installed. Mark it resolved to let paused agents start.</p>}
      <div class="row">
        <Button variant="primary" disabled={run?.running} onClick={() => store.intent({ kind: 'install_profile', node: b.node, name: profile })}>Retry install</Button>
        <a class="btn ghost" href={nodeHref(b.node)}>Edit install command</a>
        {paused.length > 0 && <ConfirmButton label="Stop the paused agents" confirmLabel={`Click again to stop ${paused.length}`} onConfirm={() => paused.forEach((agent) => store.intent({ kind: 'kill', agent }))} />}
        <ConfirmButton label="Mark resolved" confirmLabel="Click again to resolve" variant="ghost" onConfirm={() => store.intent({ kind: 'resolve_blocker', id: b.id })} />
      </div>
    </Card>
  );
}

/** A stand-in while the issue list is not loaded: the agent's prompt then lacks the issue text. */
const stubIssue = (f: Forward): Issue => ({ issue: f.issue, title: f.title || f.issue, body: '', author: 'unknown', trusted: false, labels: [], comments: 0, updated_ms: 0, url: '', tagged: false });

function ForwardCard({ f, onStart }: { f: Forward; onStart(i: Issue): void }) {
  const s = store.state.value;
  const running = Object.entries(s.statuses).filter(([a, st]) => st === 'Running' && a !== f.from).map(([a]) => a);
  const [to, setTo] = useState(f.to ?? '');
  const target = running.includes(to) ? to : running[0] ?? '';
  const n = f.issue.slice(f.issue.lastIndexOf('#'));
  return (
    <Card kind="Needs an owner" tone="warn" age={s.now - f.at_ms} label={`Forward of ${f.issue}`} title={<>{f.from} forwarded <span class="mono">{f.issue}</span></>}>
      {f.title && <span>{f.title}</span>}
      <span class="small">"{f.reason}" {f.note}</span>
      <div class="row">
        <Button variant="primary" onClick={() => onStart(s.issues?.list.find((i) => i.issue === f.issue) ?? stubIssue(f))}>Start an agent for {n}</Button>
        {running.length > 0 && (
          <>
            <select class="input filter" aria-label="Running agent" value={target} onChange={(e) => setTo(e.currentTarget.value)}>
              {running.map((a) => <option key={a} value={a}>{a}</option>)}
            </select>
            <Button onClick={() => store.intent({ kind: 'resolve_forward', id: f.id, to: target })}>Send to {target}</Button>
          </>
        )}
        <Button variant="ghost" onClick={() => store.intent({ kind: 'resolve_forward', id: f.id, to: null })}>Leave it in Issues</Button>
      </div>
    </Card>
  );
}

export function Inbox() {
  const s = store.state.value;
  const forwards = waitingForwards(s);
  const [starting, setStarting] = useState<Issue | null>(null);
  const open = s.conn === 'open';
  // A forward's "Start an agent" wants the issue text: load the list once if there is one to answer.
  useEffect(() => {
    if (open && forwards.length > 0 && s.issues === null && s.credentials.length > 0) store.intent({ kind: 'list_issues' });
  }, [open, forwards.length > 0, s.credentials.length]);
  const empty = s.questions.length + s.blockers.length + forwards.length === 0;
  return (
    <>
      <header class="page-head">
        <div>
          <h1>Inbox</h1>
          <p class="muted small">Things only you can unblock. Each card says what is stuck and has the button that fixes it.</p>
        </div>
        <a class="btn ghost" href="#/mailbox">Message log</a>
      </header>
      <div class="stack">
        {empty && <EmptyState title="Nothing waiting">Questions from agents, forwarded issues nobody owns, and login or install problems show up here.</EmptyState>}
        {s.blockers.filter((b) => b.kind === 'Install').map((b) => <BlockerCard key={b.id} b={b} />)}
        {forwards.map((f) => <ForwardCard key={f.id} f={f} onStart={setStarting} />)}
        {s.questions.map((q) => <QuestionCard key={q.id} q={q} />)}
        {s.blockers.filter((b) => b.kind === 'Login').map((b) => <BlockerCard key={b.id} b={b} />)}
      </div>
      {starting && <IssueAgent issue={starting} onClose={() => setStarting(null)} />}
    </>
  );
}
