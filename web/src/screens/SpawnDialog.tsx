import { useEffect, useRef, useState } from 'preact/hooks';
import type { Blocker, NodeInfo } from '../proto';
import {
  agentIdFromPrompt, blockersFor, defaultSpawnForm, kindTitle, lacksProfile, profileChoices, repoHint, store, takenIds,
  type State,
} from '../state';
import { Button, TextField } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { Dialog } from '../ui/Dialog';
import { AGENT_ID, nodeStateLabel } from '../ui/format';
import { Segmented } from '../ui/Segmented';
import { AddCredential, removeCredential } from './Credentials';
import { FolderPicker, type FolderChoice } from './FolderPicker';
import { RepoPicker, type RepoChoice } from './RepoPicker';

/** Offered while no node reports named profiles (older daemons). */
const LEGACY = ['claude', 'antigravity', 'custom'];
const LAST_PROFILE = 'ulak.spawn.profile';

function lastProfile(): string | null {
  try {
    return localStorage.getItem(LAST_PROFILE);
  } catch {
    return null; // storage blocked: no remembered profile
  }
}
function rememberProfile(p: string) {
  try {
    localStorage.setItem(LAST_PROFILE, p);
  } catch {
    // storage blocked: the default stays the first profile
  }
}

/** Picker values: reported profile names plus custom (a binary path), else the legacy list. */
export function profileOptions(s: State): string[] {
  const names = profileChoices(s).map((c) => c.name);
  return names.length === 0 ? LEGACY : [...names, 'custom'];
}

/** The key the hub files blockers under: binary file name, plus `@config dir`. */
export function profileKey(binary: string, configDir: string): string | null {
  const name = binary.trim().split('/').pop() ?? '';
  const dir = configDir.trim();
  return name ? (dir ? `${name}@${dir}` : name) : null;
}

/** Port of App::spawn_blocker: a blocker on the chosen node, or (Auto) on every idle node. */
export function spawnBlocker(s: State, node: string, key: string | null): Blocker | null {
  if (!key) return null;
  if (node) return blockersFor(s, node, key)[0] ?? null;
  const idle = s.nodes.filter((n) => n.state === 'Idle');
  const all = idle.every((n) => blockersFor(s, n.id, key).length > 0);
  return all ? (blockersFor(s, undefined, key).find((b) => idle.some((n) => n.id === b.node)) ?? null) : null;
}

function targetNote(s: State, n: NodeInfo, key: string | null, profile: string): string {
  const b = key ? blockersFor(s, n.id, key)[0] : undefined;
  if (b) return ` - ${kindTitle(b.kind).toLowerCase()}`;
  if (lacksProfile(s, n.id, profile)) return ` - no ${profile}`;
  if (s.nodeProfiles[n.id]?.find((p) => p.name === profile)?.login_blocked) return ' - login required';
  return n.state === 'Idle' ? '' : ` - not eligible: ${nodeStateLabel(n.state)}`;
}

/** Where the agent works: a GitHub repository (cloned on the node) or a folder on one node. */
export type Where =
  | ({ kind: 'repo'; credential: string; allowPush: boolean; fresh: boolean } & RepoChoice)
  | ({ kind: 'folder' } & FolderChoice);

type Tab = 'repo' | 'folder';

/** The Where picker: Repository or Folder on a node; reports the choice, keeps nothing itself. */
function WherePicker({ where, onPick }: { where: Where | null; onPick(w: Where): void }) {
  const s = store.state.value;
  const [tab, setTab] = useState<Tab>(where?.kind ?? 'repo');
  const [credId, setCredId] = useState(where?.kind === 'repo' ? where.credential : '');
  const [adding, setAdding] = useState(false);
  const [choice, setChoice] = useState<RepoChoice | null>(null);
  const [allowPush, setAllowPush] = useState(where?.kind === 'repo' && where.allowPush);
  const [fresh, setFresh] = useState(where?.kind === 'repo' && where.fresh);
  const [since] = useState(() => store.state.peek().nextToastId);
  const cred = s.credentials.find((c) => c.id === credId) ?? s.credentials[0];
  const known = useRef(s.credentials.length);
  // A newly added token becomes the chosen one.
  useEffect(() => {
    if (s.credentials.length > known.current) {
      setCredId(s.credentials[s.credentials.length - 1]!.id);
      setAdding(false);
    }
    known.current = s.credentials.length;
  }, [s.credentials]);
  const listError = s.toasts.filter((t) => t.kind === 'error' && t.id >= since).pop()?.text;
  const hint = choice ? repoHint(s, choice.repo) : null;

  return (
    <div class="stack">
      <Segmented<Tab> label="Source" value={tab} onChange={setTab} options={[{ value: 'repo', label: 'GitHub repository' }, { value: 'folder', label: 'Folder on a node' }]} />
      {tab === 'folder' && <FolderPicker initial={where?.kind === 'folder' ? where : null} onPick={(f) => onPick({ kind: 'folder', ...f })} />}
      {tab === 'repo' && (!cred || adding) && <AddCredential onCancel={cred ? () => setAdding(false) : undefined} />}
      {tab === 'repo' && cred && !adding && (
        <div class="stack tight">
          <div class="row">
            {s.credentials.length > 1 ? (
              <select class="input" aria-label="Credential" value={cred.id} onChange={(e) => setCredId(e.currentTarget.value)}>
                {s.credentials.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
              </select>
            ) : null}
            <span>GitHub · {cred.login}</span>
            <Button size="sm" variant="ghost" onClick={() => setAdding(true)}>Change</Button>
            <ConfirmButton label="Remove" confirmLabel="Click again to remove" onConfirm={() => removeCredential(cred)} />
          </div>
          <RepoPicker key={cred.id} credential={cred.id} error={listError} onChoice={setChoice} />
          <label class="row">
            <input type="checkbox" role="switch" checked={allowPush} onChange={(e) => setAllowPush(e.currentTarget.checked)} />
            Allow this agent to push
          </label>
          {allowPush && <p class="warn-note small">The agent will hold your GitHub token and can push with it.</p>}
          <label class="row">
            <input type="checkbox" role="switch" checked={fresh} onChange={(e) => setFresh(e.currentTarget.checked)} />
            Fresh clone
          </label>
          <p class="hint small">{fresh ? 'Always clones anew.' : 'Off: reuse an existing clone on the node when possible.'}</p>
          {hint && <p class="muted small" role="status">{hint}</p>}
          <div class="row end">
            <Button variant="primary" disabled={!choice} onClick={() => choice && onPick({ kind: 'repo', credential: cred.id, allowPush, fresh, ...choice })}>
              {choice ? `Use ${choice.repo.split('/').pop()} @ ${choice.branch}` : 'Pick a repository'}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

export const whereLabel = (w: Where | null, s: State): string =>
  !w ? 'Choose a repository or a folder' : w.kind === 'repo' ? `${w.repo} @ ${w.branch}` : `${w.path} on ${s.nodes.find((n) => n.id === w.node)?.hostname ?? w.node}`;

export function SpawnDialog({ onClose }: { onClose(): void }) {
  const s = store.state.value;
  const options = profileOptions(s);
  const [picked, setProfile] = useState(lastProfile);
  const profile = picked !== null && options.includes(picked) ? picked : options[0]!;
  const offering = profileChoices(s).find((c) => c.name === profile)?.on ?? [];
  const [view, setView] = useState<'form' | 'where'>('form');
  const [where, setWhere] = useState<Where | null>(null);
  const [prompt, setPrompt] = useState('');
  /** null: derived from the prompt until the user types one. */
  const [idEdit, setIdEdit] = useState<string | null>(null);
  const [node, setNode] = useState('');
  const [configDir, setConfigDir] = useState('');
  const [custom, setCustom] = useState('');
  const [more, setMore] = useState(false);
  // Hub errors that arrive after this toast id belong to our spawn; null = nothing in flight.
  const [pending, setPending] = useState<{ id: string; since: number; prev: unknown } | null>(null);
  const [taken] = useState(() => takenIds(store.state.peek()));

  const agentId = idEdit ?? agentIdFromPrompt(prompt, taken);
  const binary = profile === 'custom' ? custom : profile;
  const folder = where?.kind === 'folder' ? where : null;
  const target = folder ? folder.node : node;
  const dir = folder ? configDir : '';
  const key = profileKey(binary, dir);
  const blocker = spawnBlocker(s, target, key);
  const idError = agentId === '' ? 'Agent id is required.' : !AGENT_ID.test(agentId) ? 'Use 1-64 characters: lowercase letters, digits and "-".' : null;
  const error = pending ? s.toasts.find((t) => t.kind === 'error' && t.id >= pending.since)?.text : undefined;
  const toQueue = !s.nodes.some((n) => (target === '' || n.id === target) && n.state === 'Idle' && !lacksProfile(s, n.id, binary.trim()) && !(key && blockersFor(s, n.id, key).length > 0));
  const queueing = toQueue && blocker === null;
  const busy = pending !== null && !error;
  const autoSkips = !target && key && !blocker ? [...new Set(blockersFor(s, undefined, key).map((b) => b.node))] : [];
  // A field hidden under More options that blocks the start opens it.
  const moreOpen = more || (idEdit !== null && idError !== null);

  // The agent showed up: done. Select it so its terminal opens. Wait until a node lists it too: the hub refuses an attach to an agent it has not mapped to a node yet.
  useEffect(() => {
    const now = pending ? s.statuses[pending.id] : undefined;
    if (pending && s.queue.some((q) => q.agent === pending.id)) onClose();
    else if (pending && now !== undefined && now !== pending.prev && s.nodes.some((n) => n.agents.includes(pending.id))) {
      // A failed agent has nothing to attach to (the failure toast says why).
      if (now !== 'Failed') store.intent({ kind: 'select', agent: pending.id });
      onClose();
    }
  }, [pending, s.statuses, s.queue, onClose]);

  const submit = () => {
    for (const t of s.toasts) if (t.kind === 'error') store.dispatch({ kind: 'dismiss_toast', id: t.id });
    const form = {
      ...defaultSpawnForm(),
      node: target,
      agentId,
      prompt,
      binary: binary.trim(),
      configDir: dir,
      workdir: folder ? folder.path : '',
      repo: where?.kind === 'repo' ? { credential: where.credential, repo: where.repo, branch: where.branch, allowPush: where.allowPush, fresh: where.fresh } : undefined,
    };
    if (store.intent({ kind: 'spawn', form })) {
      if (profile !== 'custom') rememberProfile(profile);
      setIdEdit(agentId); // frozen: the pending spawn is tracked by this id
      setPending({ id: agentId, since: store.state.peek().nextToastId, prev: s.statuses[agentId] });
    }
  };

  // A bad id opens More options (moreOpen), so its own message is in view; the footer covers the rest.
  const why = !where ? 'Choose where it works.' : null;
  const host = (id: string) => s.nodes.find((n) => n.id === id)?.hostname ?? id;
  const summary = queueing ? `Waits in the queue as ${agentId || '...'}.` : `Starts as ${agentId || '...'} on ${target ? host(target) : 'a free node'}.`;

  if (view === 'where') {
    return (
      <Dialog title="Where should it work?" onClose={onClose} actions={<Button variant="ghost" onClick={() => setView('form')}>Back</Button>}>
        <WherePicker where={where} onPick={(w) => { setWhere(w); setView('form'); }} />
      </Dialog>
    );
  }
  return (
    <Dialog
      title="New agent"
      onClose={onClose}
      actions={
        <>
          <span class="muted small grow" role="status">{why ?? summary}</span>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={busy || blocker !== null || !where} onClick={submit}>
            {queueing ? (busy ? 'Adding...' : 'Add to queue') : busy ? 'Starting...' : 'Start agent'}
          </Button>
        </>
      }
    >
      <div class="field">
        <label for="spawn-prompt">What should it do?</label>
        <textarea id="spawn-prompt" class="input" rows={4} placeholder="Describe the task" value={prompt} onInput={(e) => setPrompt(e.currentTarget.value)} />
      </div>
      <div class="field">
        <span class="label" id="spawn-where">Where</span>
        <button type="button" class="where-field" aria-labelledby="spawn-where spawn-where-val" onClick={() => setView('where')}>
          <span id="spawn-where-val" class={`mono grow${where ? '' : ' muted'}`}>{whereLabel(where, s)}</span>
          <span class="accent small">{where ? 'Change' : 'Choose'}</span>
        </button>
      </div>
      <div class="two-col">
        <div class="field">
          <label for="spawn-agent">Agent</label>
          <select id="spawn-agent" class="input" value={profile} onChange={(e) => setProfile(e.currentTarget.value)}>
            {options.map((o) => <option key={o} value={o}>{o}</option>)}
          </select>
          {offering.length > 0 && (
            <span class="hint">
              On {offering.map(({ node: n, info }) => [n.hostname, info.from_hub && 'from hub', info.config_dir && `config ${info.config_dir}`, !info.installed && 'not installed, installs on first start', info.login_blocked && 'login required'].filter(Boolean).join(': ')).join('; ')}
            </span>
          )}
        </div>
        <div class="field">
          <label for="spawn-node">Run on</label>
          <select id="spawn-node" class="input" value={target} onChange={(e) => { if (folder) setWhere(null); setNode(e.currentTarget.value); }}>
            <option value="">A free node</option>
            {s.nodes.map((n) => <option key={n.id} value={n.id}>{n.hostname}{targetNote(s, n, key, binary.trim())}</option>)}
          </select>
          {folder && <span class="hint">The folder is on {host(folder.node)}; another node clears it.</span>}
        </div>
      </div>
      {profile === 'custom' && <TextField label="Binary" value={custom} placeholder="/usr/local/bin/my-agent" mono onValue={setCustom} />}
      <details open={moreOpen} onToggle={(e) => setMore((e.currentTarget as HTMLDetailsElement).open)}>
        <summary>More options: name{folder ? ', config folder' : ''}</summary>
        <div class="stack tight">
          <TextField label="Agent id" required value={agentId} mono error={idEdit !== null ? idError : null} autocapitalize="off" spellcheck={false} onValue={setIdEdit} hint={idEdit === null ? 'From the prompt; type to change it.' : undefined} />
          {folder && <TextField label="Config folder (CLAUDE_CONFIG_DIR)" value={configDir} placeholder="~/.claude-work" mono onValue={setConfigDir} />}
        </div>
      </details>
      {blocker && <p class="warn-note" role="alert">{kindTitle(blocker.kind)} on {blocker.node} ({blocker.profile}): {blocker.detail.slice(0, 120)} Resolve it in the Inbox first.</p>}
      {autoSkips.length > 0 && <p class="warn-note small">A free node skips {autoSkips.join(', ')} because of blockers for {key}.</p>}
      {queueing && <p class="queue-note">No node is free right now. This agent will wait in the queue and start by itself when one is.</p>}
      {error && <p class="err" role="alert">{error}</p>}
    </Dialog>
  );
}
