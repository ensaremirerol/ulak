import { useState } from 'preact/hooks';
import type { IssueSettings, RouteRule, Routing as RoutingCfg } from '../proto';
import { store, useHubAck } from '../state';
import { Button, Card, TextField } from '../ui/components';
import { Dialog } from '../ui/Dialog';
import { AGENT_ID, ageLabel } from '../ui/format';

const REPO = /^[A-Za-z0-9-]{1,39}\/[A-Za-z0-9._-]{1,100}$/;

export const ruleMatch = (r: RouteRule): string =>
  [r.repo, r.label && `label ${r.label}`, r.words.length > 0 && `title: ${r.words.join(' or ')}`].filter(Boolean).join(', ') || 'any issue';

function RuleDialog({ rule, onSave, onClose }: { rule: RouteRule | null; onSave(r: RouteRule): void; onClose(): void }) {
  const [repo, setRepo] = useState(rule?.repo ?? '');
  const [label, setLabel] = useState(rule?.label ?? '');
  const [words, setWords] = useState(rule?.words.join(', ') ?? '');
  const [to, setTo] = useState(rule?.to ?? '');
  const [fallback, setFallback] = useState(rule?.fallback ?? 'inbox');
  const repoErr = repo.trim() && !REPO.test(repo.trim()) ? 'Write it as owner/name, or leave it empty for any.' : null;
  const toErr = to && !AGENT_ID.test(to) ? 'An agent id: lowercase letters, digits and "-".' : null;
  const ok = !repoErr && !toErr && to !== '';
  const save = () => onSave({
    repo: repo.trim() || null,
    label: label.trim() || null,
    words: words.split(',').map((w) => w.trim()).filter(Boolean),
    to,
    fallback,
  });
  return (
    <Dialog title={rule ? 'Edit rule' : 'Add rule'} onClose={onClose} actions={<><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" disabled={!ok} onClick={save}>Save rule</Button></>}>
      <p class="muted small">When the issue matches every field you fill in, it goes to the agent.</p>
      <TextField label="Repository" value={repo} placeholder="owner/name (empty: any)" mono error={repoErr} onValue={setRepo} />
      <div class="two-col">
        <TextField label="Label" value={label} placeholder="docs (empty: any)" onValue={setLabel} />
        <TextField label="Title words" value={words} placeholder="readme, typo (any of them)" onValue={setWords} />
      </div>
      <TextField label="Send it to" value={to} placeholder="docs-sync" mono error={toErr} onValue={setTo} hint="The agent's id." />
      <div class="field">
        <label for="rule-fallback">If that agent is not running</label>
        <select id="rule-fallback" class="input" value={fallback} onChange={(e) => setFallback(e.currentTarget.value as RouteRule['fallback'])}>
          <option value="inbox">Ask me in Inbox</option>
          <option value="new_agent">Start a new agent for it</option>
        </select>
      </div>
    </Dialog>
  );
}

/** Settings › Issue routing: the ulak-route skill, owner rules, limits and the forward log. */
export function Routing({ settings }: { settings: IssueSettings }) {
  const s = store.state.value;
  const [r, setR] = useState<RoutingCfg>(settings.routing);
  const [editing, setEditing] = useState<number | 'new' | null>(null);
  const { ack, start } = useHubAck();
  const dirty = JSON.stringify(r) !== JSON.stringify(settings.routing);
  const save = (next: RoutingCfg) => {
    if (store.intent({ kind: 'save_issue_settings', settings: { ...settings, routing: next } })) start();
  };
  const saveRule = (rule: RouteRule) => {
    const rules = editing === 'new' ? [...r.rules, rule] : r.rules.map((x, i) => (i === editing ? rule : x));
    setR({ ...r, rules });
    setEditing(null);
  };
  const move = (i: number, d: -1 | 1) => {
    const rules = r.rules.slice();
    const [x] = rules.splice(i, 1);
    rules.splice(i + d, 0, x!);
    setR({ ...r, rules });
  };
  const maxOk = Number.isInteger(r.max_forwards) && r.max_forwards >= 1 && r.max_forwards <= 10;

  return (
    <div class="stack">
      <Card title="ulak-route skill">
        <p class="muted small">Lets an agent hand an issue, or part of its task, to the agent that owns that area. ulak adds one tool and a short block to the prompt of every agent that has the skill, and fills in the list of owners when the agent starts.</p>
        <label class="row nowrap"><input type="checkbox" checked={r.on_by_default} onChange={(e) => setR({ ...r, on_by_default: e.currentTarget.checked })} /> On for new issue agents</label>
        <div class="two-col">
          <div class="stack tight">
            <span class="label">Tool the agent gets</span>
            <pre class="cmd small">{'ulak_forward_issue(\n  issue: "owner/name#64",\n  to: "auto" | "<agent id>" | "new",\n  reason: "why this belongs elsewhere"\n)\n→ { forwarded_to: "<agent>" }\n  or { waiting_for: "you" }'}</pre>
          </div>
          <div class="field">
            <label for="route-block">Prompt block added by ulak-route</label>
            <textarea id="route-block" class="input mono small" rows={9} value={r.block} onInput={(e) => setR({ ...r, block: e.currentTarget.value })} />
            <span class="hint"><span class="mono">{'{owners}'}</span> becomes the rules, the area hints and the running agents.</span>
          </div>
        </div>
      </Card>
      <Card title="Who owns what" aside={<Button size="sm" onClick={() => setEditing('new')}>Add rule</Button>}>
        <p class="muted small">First match wins; <span class="mono">to: "auto"</span> uses it.</p>
        {r.rules.length === 0 ? <p class="muted small">No rules: "auto" forwards come to your Inbox.</p> : (
          <div class="table-wrap">
            <table>
              <thead><tr><th>When the issue matches</th><th>Send it to</th><th>If that agent is not running</th><th class="actions"><span class="sr-only">Actions</span></th></tr></thead>
              <tbody>
                {r.rules.map((rule, i) => (
                  <tr key={i}>
                    <td>{ruleMatch(rule)}</td>
                    <td class="mono">{rule.to}</td>
                    <td>{rule.fallback === 'new_agent' ? 'Start a new agent' : 'Ask me in Inbox'}</td>
                    <td class="actions">
                      <Button size="sm" variant="ghost" disabled={i === 0} aria-label={`Move rule ${i + 1} up`} onClick={() => move(i, -1)}>↑</Button>
                      <Button size="sm" variant="ghost" onClick={() => setEditing(i)}>Edit</Button>
                      <Button size="sm" variant="ghost" onClick={() => setR({ ...r, rules: r.rules.filter((_, j) => j !== i) })}>Remove</Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <div class="field">
          <label for="route-hints">Area hints for agents</label>
          <textarea id="route-hints" class="input mono small" rows={4} placeholder={'crates/api/** → api-refactor\nweb/** → web-polish'} value={r.hints} onInput={(e) => setR({ ...r, hints: e.currentTarget.value })} />
          <span class="hint">ulak cannot know which files an issue touches, so these are never matched. They go into {'{owners}'} for agents to judge while they work.</span>
        </div>
      </Card>
      <Card title="Limits">
        <div class="row">
          <label for="route-max">An issue can be forwarded at most</label>
          <input id="route-max" class="input num" type="number" min={1} max={10} value={r.max_forwards} onInput={(e) => setR({ ...r, max_forwards: Number(e.currentTarget.value) })} />
          <span>times, then it comes to Inbox.</span>
        </div>
        {!maxOk && <p class="err small">1 to 10.</p>}
        <p class="muted small">An agent is never handed an issue it already forwarded, so A → B → A cannot happen. Issues from tagged repositories come to Inbox instead of starting a new agent.</p>
        <label class="row nowrap"><input type="checkbox" checked={r.ask_first} onChange={(e) => setR({ ...r, ask_first: e.currentTarget.checked })} /> Ask me in Inbox before any forward goes through</label>
      </Card>
      {ack.kind === 'error' && <p class="err" role="alert">{ack.message}</p>}
      <div class="row end">
        {dirty && <Button variant="ghost" onClick={() => setR(settings.routing)}>Discard changes</Button>}
        <Button variant="primary" disabled={!dirty || !maxOk || ack.kind === 'waiting'} onClick={() => save(r)}>{ack.kind === 'waiting' ? 'Saving...' : 'Save routing'}</Button>
      </div>
      <Card title="Recent forwards">
        {s.forwards.length === 0 ? <p class="muted small">No forwards yet.</p> : (
          <ul class="plain" aria-label="Recent forwards">
            {s.forwards.slice(0, 20).map((f) => (
              <li key={f.id} class="msg">
                <span class="row small"><span class="mono">{f.from} → {f.to ?? 'you'}</span><span class="mono muted">{f.issue}</span><span class="grow" /><span class="muted">{f.status.replace('_', ' ')} · {ageLabel(s.now - f.at_ms)}</span></span>
                <span class="small">{f.reason}</span>
              </li>
            ))}
          </ul>
        )}
      </Card>
      {editing !== null && <RuleDialog rule={editing === 'new' ? null : r.rules[editing]!} onSave={saveRule} onClose={() => setEditing(null)} />}
    </div>
  );
}
