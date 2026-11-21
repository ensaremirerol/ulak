import { useEffect, useState } from 'preact/hooks';
import type { Issue } from '../proto';
import { defaultSpawnForm, issueNumber, renderTemplate, store, suggestedTemplate, takenIds, type State } from '../state';
import { Button, TextField } from '../ui/components';
import { Dialog } from '../ui/Dialog';
import { AGENT_ID, nodeStateLabel } from '../ui/format';
import { profileOptions } from './SpawnDialog';

const NONE = '';
export const repoOfIssue = (ref: string): string => ref.slice(0, ref.lastIndexOf('#'));

/** `issue-64`, or `issue-64-2`... when taken. */
export function issueAgentId(n: number, taken: Set<string>): string {
  for (let i = 1; ; i++) {
    const id = i === 1 ? `issue-${n}` : `issue-${n}-${i}`;
    if (!taken.has(id)) return id;
  }
}

export interface IssueSpawn {
  issue: Issue;
  prompt: string;
  branch: string;
  agentId: string;
  profile: string;
  node: string;
  route: boolean;
  comment: boolean;
}

/** Sends the spawn (repository source, may push). False if no credential or the form is refused. */
export function sendIssueSpawn(s: State, x: IssueSpawn): boolean {
  const cred = s.issueSettings?.credential ?? s.credentials[0]?.id;
  if (!cred) return false;
  const form = {
    ...defaultSpawnForm(),
    node: x.node,
    agentId: x.agentId,
    prompt: x.prompt,
    binary: x.profile,
    repo: { credential: cred, repo: repoOfIssue(x.issue.issue), branch: '', allowPush: true },
  };
  return store.intent({ kind: 'spawn_for_issue', form, issue: { issue: x.issue.issue, branch: x.branch.trim() || null, comment: x.comment, route: x.route } });
}

/** "Start agent" without review: the template's defaults on a free node. */
export function startIssueAgent(s: State, issue: Issue, templateId: string): boolean {
  const t = s.issueSettings?.templates.find((x) => x.id === templateId);
  if (!t) return false;
  const n = issueNumber(issue.issue);
  const branch = `issue-${n}`;
  return sendIssueSpawn(s, {
    issue,
    prompt: renderTemplate(t.prompt, issue, branch),
    branch,
    agentId: issueAgentId(n, takenIds(s)),
    profile: profileOptions(s).find((o) => o !== 'custom') ?? 'claude',
    node: '',
    route: t.route || (s.issueSettings?.routing.on_by_default ?? false),
    comment: t.comment,
  });
}

/** New agent for an issue: a prompt filled from a template, then Start. */
export function IssueAgent({ issue, onClose, templateId }: { issue: Issue; onClose(): void; templateId?: string }) {
  const s = store.state.value;
  const settings = s.issueSettings;
  const templates = settings?.templates ?? [];
  const n = issueNumber(issue.issue);
  const [tpl, setTpl] = useState(templateId ?? suggestedTemplate(templates, issue.labels)?.id ?? NONE);
  const [branch, setBranch] = useState(`issue-${n}`);
  const chosen = templates.find((t) => t.id === tpl) ?? null;
  const [prompt, setPrompt] = useState(() => (chosen ? renderTemplate(chosen.prompt, issue, branch) : ''));
  const options = profileOptions(s);
  const [profile, setProfile] = useState(options.find((o) => o !== 'custom') ?? 'claude');
  const [node, setNode] = useState('');
  const [route, setRoute] = useState(chosen?.route ?? settings?.routing.on_by_default ?? false);
  const [comment, setComment] = useState(chosen?.comment ?? false);
  const [taken] = useState(() => takenIds(store.state.peek()));
  const [agentId, setAgentId] = useState(() => issueAgentId(n, taken));
  const [pending, setPending] = useState<{ id: string; since: number } | null>(null);
  const cred = settings?.credential ?? s.credentials[0]?.id ?? null;

  const pickTemplate = (id: string) => {
    setTpl(id);
    const t = templates.find((x) => x.id === id);
    if (t) {
      setPrompt(renderTemplate(t.prompt, issue, branch));
      setRoute(t.route || (settings?.routing.on_by_default ?? false));
      setComment(t.comment);
    }
  };
  const changeBranch = (b: string) => {
    setBranch(b);
    if (chosen) setPrompt(renderTemplate(chosen.prompt, issue, b));
  };

  const error = pending ? s.toasts.find((t) => t.kind === 'error' && t.id >= pending.since)?.text : undefined;
  const busy = pending !== null && !error;
  useEffect(() => {
    if (pending && s.issueLinks.some((l) => l.agent === pending.id)) onClose();
  }, [pending, s.issueLinks, onClose]);

  const idError = !AGENT_ID.test(agentId) ? 'Use 1-64 characters: lowercase letters, digits and "-".' : null;
  const why = !cred ? 'Add a GitHub credential in Settings first.' : !prompt.trim() ? 'The prompt is empty.' : idError;
  const host = node ? (s.nodes.find((x) => x.id === node)?.hostname ?? node) : 'a free node';

  const submit = () => {
    const since = store.state.peek().nextToastId;
    if (sendIssueSpawn(s, { issue, prompt, branch, agentId, profile, node, route, comment })) setPending({ id: agentId, since });
  };

  return (
    <Dialog
      title="New agent for an issue"
      onClose={onClose}
      actions={
        <>
          <span class="muted small grow" role="status">{why ?? `Starts as ${agentId} on ${host}, linked to #${n}.`}</span>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={busy || why !== null} onClick={submit}>{busy ? 'Starting...' : 'Start agent'}</Button>
        </>
      }
    >
      <div class="issue-head">
        <span class="mono small muted">{issue.issue}</span>
        <strong>{issue.title}</strong>
        {issue.labels.length > 0 && <span class="chips">{issue.labels.map((l) => <span key={l} class="label-chip">{l}</span>)}</span>}
      </div>
      {!issue.trusted && <p class="warn-note small" role="alert">{issue.author} is outside this repository. Read the prompt before you start: the issue text goes to the agent as data, but it can still try to mislead it.</p>}
      <div class="field">
        <div class="row">
          <label for="ia-template" class="grow">Template</label>
          <a class="small" href="#/settings/templates">Edit templates</a>
        </div>
        <select id="ia-template" class="input" value={tpl} onChange={(e) => pickTemplate(e.currentTarget.value)}>
          {templates.map((t) => <option key={t.id} value={t.id}>{t.name}</option>)}
          <option value={NONE}>No template (write it yourself)</option>
        </select>
      </div>
      <div class="field">
        <label for="ia-prompt">Prompt</label>
        <textarea id="ia-prompt" class="input mono small" rows={10} value={prompt} onInput={(e) => setPrompt(e.currentTarget.value)} />
        <span class="hint">Filled from the template. Edits here apply to this agent only.</span>
      </div>
      <div class="two-col">
        <TextField label="Branch" value={branch} mono autocapitalize="off" spellcheck={false} onValue={changeBranch} hint="The prompt names it; ulak watches it for a PR." />
        <TextField label="Agent id" value={agentId} mono autocapitalize="off" spellcheck={false} error={idError} onValue={setAgentId} />
      </div>
      <div class="two-col">
        <div class="field">
          <label for="ia-agent">Agent</label>
          <select id="ia-agent" class="input" value={profile} onChange={(e) => setProfile(e.currentTarget.value)}>
            {options.filter((o) => o !== 'custom').map((o) => <option key={o} value={o}>{o}</option>)}
          </select>
        </div>
        <div class="field">
          <label for="ia-node">Run on</label>
          <select id="ia-node" class="input" value={node} onChange={(e) => setNode(e.currentTarget.value)}>
            <option value="">A free node</option>
            {s.nodes.map((x) => <option key={x.id} value={x.id}>{x.hostname}{x.state === 'Idle' ? '' : ` - ${nodeStateLabel(x.state)}`}</option>)}
          </select>
        </div>
      </div>
      <fieldset class="stack tight plain-fieldset">
        <legend class="label">Agent can</legend>
        <label class="row nowrap">
          <input type="checkbox" checked={route} onChange={(e) => setRoute(e.currentTarget.checked)} />
          <span>Forward issues to other agents <span class="mono small">ulak-route</span></span>
        </label>
        <span class="hint">If part of the work belongs to another agent, it hands it over instead of doing it. <a href="#/settings/routing">How routing works</a></span>
        <label class="row nowrap">
          <input type="checkbox" checked={comment} onChange={(e) => setComment(e.currentTarget.checked)} />
          <span>Comment on #{n} when it starts and when the PR opens</span>
        </label>
        <span class="hint">Uses your GitHub credential. The agent can push to this repository.</span>
      </fieldset>
      {error && <p class="err" role="alert">{error}</p>}
    </Dialog>
  );
}
