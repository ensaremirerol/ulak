import { useEffect, useState } from 'preact/hooks';
import type { Issue, RepoRef } from '../proto';
import { issueAgents, issueAgentState, store, suggestedTemplate, type IssueAgentState, type State } from '../state';
import { Button, EmptyState, TextField } from '../ui/components';
import { Dialog } from '../ui/Dialog';
import { ageLabel } from '../ui/format';
import { IssueAgent, repoOfIssue, startIssueAgent } from './IssueAgent';

type Scope = 'all' | 'mine' | 'tagged';
const REPO = /^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/;

function openAgent(agent: string) {
  location.hash = '#/agents';
  store.intent({ kind: 'select', agent });
}

function saveTagged(s: State, tagged: RepoRef[]) {
  if (s.issueSettings) store.intent({ kind: 'save_issue_settings', settings: { ...s.issueSettings, tagged } });
}

function TagRepo({ onClose }: { onClose(): void }) {
  const s = store.state.value;
  const [repo, setRepo] = useState('');
  const v = repo.trim().replace(/^https:\/\/github\.com\//, '').replace(/\/$/, '');
  const err = v && !REPO.test(v) ? 'Write it as owner/name.' : s.issueSettings?.tagged.includes(v) ? 'Already tagged.' : null;
  const add = () => {
    saveTagged(s, [...(s.issueSettings?.tagged ?? []), v]);
    onClose();
    store.intent({ kind: 'list_issues' });
  };
  return (
    <Dialog
      title="Tag a repository"
      onClose={onClose}
      actions={<><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" disabled={!v || err !== null} onClick={add}>Tag it</Button></>}
    >
      <p class="muted small">Its open issues show up here next to yours. Agents for them start only when you start one: never on their own.</p>
      <TextField label="Repository" value={repo} placeholder="owner/name" mono autocapitalize="off" spellcheck={false} error={err} onValue={setRepo} />
    </Dialog>
  );
}

const STATE_LABEL: Record<IssueAgentState, string> = { none: 'No agent yet', working: 'Agent working', forwarded: 'Forwarded' };

function IssueRow({ i, selected, onSelect }: { i: Issue; selected: boolean; onSelect(): void }) {
  const s = store.state.value;
  const live = issueAgents(s, i.issue).find((a) => a.live);
  return (
    <li>
      <button type="button" class="issue-row" aria-current={selected ? 'true' : undefined} onClick={onSelect}>
        <span class="row nowrap small muted">
          <span class="mono grow">{i.issue}</span>
          {i.tagged && <span>tagged{i.trusted ? '' : ' · external author'}</span>}
          <span>{ageLabel(s.now - i.updated_ms)}</span>
        </span>
        <span class="issue-title">{i.title}</span>
        <span class="row small">
          {i.labels.map((l) => <span key={l} class="label-chip">{l}</span>)}
          <span class="grow" />
          {live ? <span class="status ok">{live.agent}</span> : issueAgentState(s, i.issue) === 'forwarded' ? <span class="status info">forwarded</span> : null}
        </span>
      </button>
    </li>
  );
}

function IssueDetail({ i, onStart }: { i: Issue; onStart(templateId: string, review: boolean): void }) {
  const s = store.state.value;
  const templates = s.issueSettings?.templates ?? [];
  const [tpl, setTpl] = useState(() => suggestedTemplate(templates, i.labels)?.id ?? '');
  useEffect(() => setTpl(suggestedTemplate(templates, i.labels)?.id ?? ''), [i.issue]);
  const agents = issueAgents(s, i.issue);
  const pr = agents.find((a) => a.pr !== null)?.pr;
  const forwards = s.forwards.filter((f) => f.issue === i.issue);
  const suggested = suggestedTemplate(templates, i.labels)?.id;
  return (
    <section class="agent-panel" aria-label={`Issue ${i.issue}`}>
      <div class="panel-head">
        <div class="panel-title">
          <span class="mono small muted">{i.issue}</span>
          <strong>{i.title}</strong>
          <span class="muted small">Opened by {i.author}{i.trusted ? '' : ' (outside the repository)'} · {i.comments} comments · {pr ? `PR #${pr}` : 'no PR'}</span>
        </div>
        <a class="btn sm ghost" href={i.url} target="_blank" rel="noreferrer noopener">Open on GitHub</a>
      </div>
      <div class="panel-body stack">
        <div class="issue-body pre-wrap small">{i.body.trim() || <span class="muted">No description.</span>}</div>
        {agents.length > 0 && (
          <div class="stack tight">
            <h3 class="group-title">Agents</h3>
            {agents.map((a) => (
              <span key={a.agent} class="row small">
                <button type="button" class="linkish" onClick={() => openAgent(a.agent)}>{a.agent}</button>
                <span class="muted">{a.live ? 'working' : 'ended'}{a.pr ? ` · opened PR #${a.pr}` : ''}</span>
              </span>
            ))}
          </div>
        )}
        {forwards.length > 0 && (
          <div class="stack tight">
            <h3 class="group-title">Forwards</h3>
            {forwards.map((f) => <span key={f.id} class="small">{f.from} → {f.to ?? 'you'}: {f.reason} <span class="muted">({f.status.replace('_', ' ')})</span></span>)}
          </div>
        )}
        <div class="stack tight start-box">
          <h3 class="group-title">Start an agent for this issue</h3>
          {templates.length === 0 ? <p class="muted small">No templates yet. <a href="#/settings/templates">Add one</a>, or review and write the prompt yourself.</p> : (
            <div class="field">
              <label for="is-template">Template</label>
              <select id="is-template" class="input" value={tpl} onChange={(e) => setTpl(e.currentTarget.value)}>
                {templates.map((t) => <option key={t.id} value={t.id}>{t.name}{t.id === suggested && t.labels.length > 0 ? ` (suggested for ${t.labels.join(', ')})` : ''}</option>)}
              </select>
            </div>
          )}
          <div class="row">
            {/* An issue from outside always goes through review first. */}
            {i.trusted && tpl && <Button variant="primary" onClick={() => onStart(tpl, false)}>Start agent</Button>}
            <Button variant={i.trusted && tpl ? 'ghost' : 'primary'} onClick={() => onStart(tpl, true)}>Review prompt first</Button>
          </div>
          <p class="hint small">
            {i.tagged ? 'Tagged repositories are manual only.' : 'Auto-start: only your repositories, only when you add the ulak label yourself.'} <a href="#/settings/templates">Change</a>
          </p>
        </div>
      </div>
    </section>
  );
}

export function Issues() {
  const s = store.state.value;
  const [scope, setScope] = useState<Scope>('all');
  const [query, setQuery] = useState('');
  const [agentState, setAgentState] = useState<'' | IssueAgentState>('');
  const [sel, setSel] = useState<string | null>(null);
  const [tagging, setTagging] = useState(false);
  const [reviewing, setReviewing] = useState<{ issue: Issue; templateId: string } | null>(null);
  const open = s.conn === 'open';
  const hasCred = s.credentials.length > 0;
  useEffect(() => {
    if (open && hasCred) store.intent({ kind: 'list_issues' });
  }, [open, hasCred]);
  useEffect(() => {
    if (open) store.intent({ kind: 'list_credentials' });
  }, [open]);

  const all = s.issues?.list ?? [];
  const q = query.trim().toLowerCase();
  const rows = all.filter((i) =>
    (scope === 'all' || (scope === 'tagged') === i.tagged)
    && (!q || i.title.toLowerCase().includes(q) || i.issue.toLowerCase().includes(q) || i.labels.some((l) => l.toLowerCase().includes(q)))
    && (agentState === '' || issueAgentState(s, i.issue) === agentState));
  const selected = rows.find((i) => i.issue === sel) ?? rows[0] ?? null;
  const repos = [...new Set(all.map((i) => repoOfIssue(i.issue)))].sort();
  const tagged = s.issueSettings?.tagged ?? [];
  const count = (sc: Scope) => all.filter((i) => sc === 'all' || (sc === 'tagged') === i.tagged).length;

  const start = (i: Issue, templateId: string, review: boolean) => {
    if (review || !i.trusted || !startIssueAgent(store.state.peek(), i, templateId)) setReviewing({ issue: i, templateId });
  };

  return (
    <>
      <header class="page-head">
        <div>
          <h1>Issues</h1>
          <p class="muted small">Open GitHub issues from your repositories and the ones you tagged. Start an agent on any of them.</p>
        </div>
        <div class="row">
          <Button variant="ghost" disabled={!hasCred} onClick={() => store.intent({ kind: 'list_issues' })}>Refresh</Button>
          <Button onClick={() => setTagging(true)} disabled={!s.issueSettings}>Tag a repo</Button>
        </div>
      </header>
      {!hasCred ? (
        <EmptyState title="No GitHub credential" action={<a class="btn primary" href="#/settings/github">Add one</a>}>Issues are read with a GitHub token you store on the hub.</EmptyState>
      ) : (
        <section class="console">
          <div class="row">
            <div class="seg" role="group" aria-label="Which issues">
              {(['all', 'mine', 'tagged'] as const).map((sc) => (
                <button key={sc} type="button" aria-pressed={scope === sc} onClick={() => setScope(sc)}>{sc === 'all' ? 'All' : sc === 'mine' ? 'My repos' : 'Tagged'} · {count(sc)}</button>
              ))}
            </div>
            <input class="input filter" type="search" aria-label="Filter issues" placeholder="Filter issues" value={query} onInput={(e) => setQuery(e.currentTarget.value)} />
            <select class="input filter" aria-label="Agent status" value={agentState} onChange={(e) => setAgentState(e.currentTarget.value as '' | IssueAgentState)}>
              <option value="">Any agent status</option>
              {(['none', 'working', 'forwarded'] as const).map((v) => <option key={v} value={v}>{STATE_LABEL[v]}</option>)}
            </select>
          </div>
          {(repos.length > 0 || tagged.length > 0) && (
            <div class="row small" aria-label="Repositories">
              <span class="muted">Repos:</span>
              {repos.filter((r) => !tagged.includes(r)).map((r) => <span key={r} class="label-chip mono">{r}</span>)}
              {tagged.map((r) => (
                <span key={r} class="label-chip mono">
                  {r} · tagged
                  <button type="button" class="chip-x" aria-label={`Untag ${r}`} onClick={() => saveTagged(s, tagged.filter((x) => x !== r))}>×</button>
                </span>
              ))}
            </div>
          )}
          {s.issues?.errors.map((e) => <p key={e} class="warn-note small" role="alert">{e}</p>)}
          {s.issues === null ? <p class="muted" role="status">Loading issues...</p> : (
            <div class="console-body">
              <div class="agent-col">
                {rows.length === 0 ? <EmptyState title="No issues">{all.length === 0 ? 'No open issues in your repositories or tagged ones.' : 'Nothing matches the filters.'}</EmptyState> : (
                  <ul class="agent-list" aria-label="Issues">
                    {rows.map((i) => <IssueRow key={i.issue} i={i} selected={selected?.issue === i.issue} onSelect={() => setSel(i.issue)} />)}
                  </ul>
                )}
              </div>
              {selected && <IssueDetail key={selected.issue} i={selected} onStart={(t, review) => start(selected, t, review)} />}
            </div>
          )}
        </section>
      )}
      {tagging && <TagRepo onClose={() => setTagging(false)} />}
      {reviewing && <IssueAgent issue={reviewing.issue} templateId={reviewing.templateId || undefined} onClose={() => setReviewing(null)} />}
    </>
  );
}
