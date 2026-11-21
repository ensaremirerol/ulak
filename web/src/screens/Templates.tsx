import { useRef, useState } from 'preact/hooks';
import type { Issue, IssueSettings, PromptTemplate } from '../proto';
import { renderTemplate, store, TEMPLATE_VARS, useHubAck } from '../state';
import { Button, TextField } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';

const SAMPLE: Issue = {
  issue: 'you/project#64', title: 'Installing an agent fails without root', body: 'npm i -g fails with EACCES when ulakd runs as a normal user.',
  author: 'you', trusted: true, labels: ['bug'], comments: 3, updated_ms: 0, url: 'https://github.com/you/project/issues/64', tagged: false,
};

const slugOf = (name: string, taken: string[]) => {
  const base = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40) || 'template';
  let id = base;
  for (let i = 2; taken.includes(id); i++) id = `${base}-${i}`;
  return id;
};

const blank = (taken: string[]): PromptTemplate => ({ id: slugOf('new template', taken), name: 'New template', labels: [], prompt: '', route: false, comment: false, auto_start: false });

/** Settings › Templates: reusable prompts for agents started from issues. */
export function Templates({ settings }: { settings: IssueSettings }) {
  const s = store.state.value;
  const [selId, setSelId] = useState(settings.templates[0]?.id ?? null);
  const [draft, setDraft] = useState<PromptTemplate | null>(settings.templates[0] ?? null);
  const [labels, setLabels] = useState((settings.templates[0]?.labels ?? []).join(', '));
  const { ack, start } = useHubAck();
  const area = useRef<HTMLTextAreaElement>(null);
  const ids = settings.templates.map((t) => t.id);
  const sample = s.issues?.list.find((i) => i.trusted) ?? SAMPLE;

  const pick = (t: PromptTemplate) => {
    setSelId(t.id);
    setDraft(t);
    setLabels(t.labels.join(', '));
  };
  const save = (templates: PromptTemplate[]) => {
    if (store.intent({ kind: 'save_issue_settings', settings: { ...settings, templates } })) start();
  };
  const current = (): PromptTemplate | null => (draft ? { ...draft, labels: labels.split(',').map((l) => l.trim()).filter(Boolean) } : null);
  const saveDraft = () => {
    const t = current();
    if (!t) return;
    const exists = settings.templates.some((x) => x.id === selId);
    save(exists ? settings.templates.map((x) => (x.id === selId ? t : x)) : [...settings.templates, t]);
    setSelId(t.id);
  };
  const add = (t: PromptTemplate) => {
    setSelId(null);
    setDraft(t);
    setLabels(t.labels.join(', '));
  };
  const insert = (v: string) => {
    if (!draft) return;
    const el = area.current;
    const at = el ? el.selectionStart : draft.prompt.length;
    const end = el ? el.selectionEnd : at;
    setDraft({ ...draft, prompt: draft.prompt.slice(0, at) + v + draft.prompt.slice(end) });
    requestAnimationFrame(() => el?.focus());
  };
  const dirty = draft !== null && JSON.stringify(current()) !== JSON.stringify(settings.templates.find((t) => t.id === selId));
  const nameError = draft && !draft.name.trim() ? 'A name is required.' : null;
  const promptError = draft && !draft.prompt.trim() ? 'The prompt is empty.' : null;

  return (
    <div class="console-body">
      <div class="agent-col">
        <Button onClick={() => add(blank(ids))}>New template</Button>
        <ul class="agent-list" aria-label="Templates">
          {settings.templates.map((t) => (
            <li key={t.id}>
              <button type="button" class="issue-row" aria-current={t.id === selId ? 'true' : undefined} onClick={() => pick(t)}>
                <span>{t.name}</span>
                <span class="muted small">{[t.labels.length ? `for ${t.labels.join(', ')}` : null, t.auto_start ? 'auto-starts' : null, t.route ? 'ulak-route' : null].filter(Boolean).join(' · ') || 'manual'}</span>
              </button>
            </li>
          ))}
        </ul>
      </div>
      {!draft ? <p class="muted">No templates. Add one to start agents from issues.</p> : (
        <section class="agent-panel" aria-label="Template">
          <div class="panel-body stack">
            <div class="two-col">
              <TextField label="Name" value={draft.name} error={nameError} onValue={(v) => setDraft({ ...draft, name: v })} />
              <TextField label="Suggest it for" value={labels} placeholder="bug, regression" hint="Labels, comma-separated. An issue with one of them picks this template." onValue={setLabels} />
            </div>
            <div class="field">
              <label for="tpl-prompt">Prompt</label>
              <textarea ref={area} id="tpl-prompt" class="input mono small" rows={10} value={draft.prompt} onInput={(e) => setDraft({ ...draft, prompt: e.currentTarget.value })} />
              {promptError && <span class="err small">{promptError}</span>}
            </div>
            <div class="stack tight">
              <span class="label">Variables <span class="muted small">Click one to insert it.</span></span>
              <div class="chips">
                {TEMPLATE_VARS.map((v) => <button key={v.name} type="button" class="label-chip var-chip" title={v.desc} onClick={() => insert(v.name)}><span class="mono">{v.name}</span> <span class="muted">{v.desc}</span></button>)}
              </div>
            </div>
            <fieldset class="stack tight plain-fieldset">
              <legend class="label">Defaults for agents started from it</legend>
              <label class="row nowrap"><input type="checkbox" checked={draft.route} onChange={(e) => setDraft({ ...draft, route: e.currentTarget.checked })} /> Give the <span class="mono">ulak-route</span> skill (forward issues to other agents)</label>
              <label class="row nowrap"><input type="checkbox" checked={draft.comment} onChange={(e) => setDraft({ ...draft, comment: e.currentTarget.checked })} /> Comment on the issue when the agent starts and when the PR opens</label>
              <label class="row nowrap"><input type="checkbox" checked={draft.auto_start} onChange={(e) => setDraft({ ...draft, auto_start: e.currentTarget.checked })} /> Auto-start when I add the label <span class="mono">ulak</span> to an issue in my repos</label>
              <span class="hint">The label counts only when your own account added it. Tagged repositories never auto-start.</span>
            </fieldset>
            <div class="stack tight">
              <span class="label">Preview with {sample.issue}</span>
              <pre class="cmd small">{renderTemplate(draft.prompt, sample, `issue-${sample.issue.split('#')[1]}`)}</pre>
            </div>
            {ack.kind === 'error' && <p class="err" role="alert">{ack.message}</p>}
            <div class="row end">
              {selId && <ConfirmButton label="Delete" confirmLabel="Click again to delete" onConfirm={() => { save(settings.templates.filter((t) => t.id !== selId)); setSelId(null); setDraft(null); }} />}
              {selId && <Button variant="ghost" onClick={() => { const t = current(); if (t) add({ ...t, id: slugOf(`${t.name} copy`, ids), name: `${t.name} (copy)` }); }}>Duplicate</Button>}
              <Button variant="primary" disabled={!dirty || nameError !== null || promptError !== null || ack.kind === 'waiting'} onClick={saveDraft}>{ack.kind === 'waiting' ? 'Saving...' : 'Save'}</Button>
            </div>
          </div>
        </section>
      )}
    </div>
  );
}
