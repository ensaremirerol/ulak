import { useEffect, useRef, useState } from 'preact/hooks';
import type { Blocker, NodeInfo } from '../proto';
import { blockersFor, defaultSpawnForm, kindTitle, lacksProfile, profileChoices, repoHint, store, type SpawnForm, type State } from '../state';
import { Button, TextField } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { Dialog } from '../ui/Dialog';
import { AGENT_ID, nodeStateLabel } from '../ui/format';
import { Segmented } from '../ui/Segmented';
import { AddCredential, removeCredential } from './Credentials';
import { RepoPicker, type RepoChoice } from './RepoPicker';

type Source = 'repo' | 'folder';

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

export function SpawnDialog({ onClose }: { onClose(): void }) {
  const s = store.state.value;
  const options = profileOptions(s);
  const [picked, setProfile] = useState(lastProfile);
  const profile = picked !== null && options.includes(picked) ? picked : options[0]!;
  const offering = profileChoices(s).find((c) => c.name === profile)?.on ?? [];
  const [tried, setTried] = useState(false);
  const [fresh, setFresh] = useState(false);
  const [source, setSource] = useState<Source>('repo');
  const [form, setForm] = useState<SpawnForm>(defaultSpawnForm());
  // Hub errors that arrive after this toast id belong to our spawn; null = nothing in flight.
  const [pending, setPending] = useState<{ id: string; since: number; prev: unknown } | null>(null);
  const [custom, setCustom] = useState('');
  const set = (k: keyof SpawnForm) => (v: string) => setForm((f) => ({ ...f, [k]: v }));

  const binary = profile === 'custom' ? custom : profile;
  const repoSource = source === 'repo';
  const configDir = repoSource ? '' : form.configDir;
  const key = profileKey(binary, configDir);
  const blocker = spawnBlocker(s, form.node.trim(), key);
  const idError = form.agentId === '' ? (tried ? 'Agent id is required.' : null) : !AGENT_ID.test(form.agentId) ? 'Use 1-64 characters: lowercase letters, digits and "-".' : null;
  const error = pending ? s.toasts.find((t) => t.kind === 'error' && t.id >= pending.since)?.text : undefined;
  const toQueue = !s.nodes.some((n) => (form.node === '' || n.id === form.node.trim()) && n.state === 'Idle' && !lacksProfile(s, n.id, binary.trim()) && !(key && blockersFor(s, n.id, key).length > 0));
  const [credId, setCredId] = useState('');
  const [adding, setAdding] = useState(false);
  const [choice, setChoice] = useState<RepoChoice | null>(null);
  const [allowPush, setAllowPush] = useState(false);
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
    setTried(true);
    if (store.intent({ kind: 'spawn', form: { ...form, configDir, binary: binary.trim(), repo: repoSource && cred && choice ? { credential: cred.id, ...choice, allowPush, fresh } : undefined } })) {
      if (profile !== 'custom') rememberProfile(profile);
      setPending({ id: form.agentId.trim(), since: store.state.peek().nextToastId, prev: s.statuses[form.agentId.trim()] });
    }
  };
  const hint = repoSource && choice ? repoHint(s, choice.repo) : null;
  const queueing = toQueue && blocker === null;
  const busy = pending !== null && !error;
  const autoSkips = !form.node && key && !blocker ? [...new Set(blockersFor(s, undefined, key).map((b) => b.node))] : [];

  return (
    <Dialog
      title="New agent"
      onClose={onClose}
      actions={
        <>
          <Button variant="ghost" onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={busy || blocker !== null || (repoSource && !(cred && choice))} onClick={submit}>
            {queueing ? (busy ? 'Adding...' : 'Add to queue') : busy ? 'Spawning...' : 'Spawn'}
          </Button>
        </>
      }
    >
      <Segmented<Source>
        label="Source"
        value={source}
        onChange={setSource}
        options={[
          { value: 'repo', label: 'Repository' },
          { value: 'folder', label: 'Folder on the node' },
        ]}
      />
      {repoSource && (
        <div class="stack tight">
          {!cred || adding ? (
            <AddCredential onCancel={cred ? () => setAdding(false) : undefined} />
          ) : (
            <>
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
              <RepoPicker key={cred.id} credential={cred.id} error={pending ? undefined : listError} onChoice={setChoice} />
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
            </>
          )}
        </div>
      )}
      <div class="field">
        <span class="label">Profile</span>
        <Segmented<string> label="Profile" value={profile} options={options.map((o) => ({ value: o, label: o }))} onChange={setProfile} />
        {offering.length > 0 && (
          <span class="hint">
            On {offering.map(({ node, info }) => [node.hostname, info.config_dir && `config ${info.config_dir}`, !info.installed && 'not installed, installs on first spawn', info.login_blocked && 'login required'].filter(Boolean).join(': ')).join('; ')}
          </span>
        )}
      </div>
      {profile === 'custom' && <TextField label="Binary" value={custom} placeholder="/usr/local/bin/my-agent" mono onValue={setCustom} />}
      {!repoSource && <TextField label="Working directory" value={form.workdir} placeholder="/home/me/project" mono onValue={set('workdir')} />}
      {!repoSource && (
        <details>
          <summary>Advanced</summary>
          <TextField label="CLAUDE_CONFIG_DIR (optional)" value={form.configDir} placeholder="~/.claude-work" mono onValue={set('configDir')} />
        </details>
      )}
      <TextField label="Agent id" required value={form.agentId} placeholder="worker-1" mono error={idError} autocapitalize="off" spellcheck={false} onValue={set('agentId')} />
      <div class="field">
        <label for="spawn-prompt">Prompt</label>
        <textarea id="spawn-prompt" class="input" rows={3} placeholder="What should the agent do?" value={form.prompt} onInput={(e) => set('prompt')(e.currentTarget.value)} />
      </div>
      <div class="field" role="group" aria-label="Target node">
        <span class="label">Run on</span>
        <div class="chips">
          {[{ id: '', label: 'Auto' }, ...s.nodes.map((n) => ({ id: n.id, label: `${n.hostname}${targetNote(s, n, key, binary.trim())}` }))].map((t) => (
            <button key={t.id} type="button" class="chip" aria-pressed={form.node === t.id} onClick={() => set('node')(t.id)}>{t.label}</button>
          ))}
        </div>
      </div>
      {blocker && <p class="warn-note" role="alert">{kindTitle(blocker.kind)} on {blocker.node} ({blocker.profile}): {blocker.detail.slice(0, 120)} Resolve it in the Inbox first.</p>}
      {autoSkips.length > 0 && <p class="warn-note small">Auto skips {autoSkips.join(', ')} because of blockers for {key}.</p>}
      {queueing && <p class="queue-note">No node is free right now. This agent will wait in the queue and start by itself when one is.</p>}
      {error && <p class="err" role="alert">{error}</p>}
    </Dialog>
  );
}
