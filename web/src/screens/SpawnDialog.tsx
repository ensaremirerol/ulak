import { useEffect, useRef, useState } from 'preact/hooks';
import type { Blocker, NodeInfo } from '../proto';
import { blockersFor, defaultSpawnForm, kindTitle, store, type SpawnForm, type State } from '../state';
import { Button, TextField } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { Dialog } from '../ui/Dialog';
import { AGENT_ID, nodeStateLabel } from '../ui/format';
import { Segmented } from '../ui/Segmented';
import { AddCredential, removeCredential } from './Credentials';
import { RepoPicker, type RepoChoice } from './RepoPicker';

type Profile = 'claude' | 'antigravity' | 'custom';
type Source = 'repo' | 'folder';

const PROFILES = [{ value: 'claude', label: 'claude' }, { value: 'antigravity', label: 'antigravity' }, { value: 'custom', label: 'custom' }] as const;

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

function targetNote(s: State, n: NodeInfo, key: string | null): string {
  const b = key ? blockersFor(s, n.id, key)[0] : undefined;
  if (b) return ` - ${kindTitle(b.kind).toLowerCase()}`;
  return n.state === 'Idle' ? '' : ` - not eligible: ${nodeStateLabel(n.state)}`;
}

export function SpawnDialog({ onClose }: { onClose(): void }) {
  const s = store.state.value;
  const [profile, setProfile] = useState<Profile>('claude');
  const [source, setSource] = useState<Source>('repo');
  const [form, setForm] = useState<SpawnForm>(defaultSpawnForm());
  // Hub errors that arrive after this toast id belong to our spawn; null = nothing in flight.
  const [pending, setPending] = useState<{ id: string; since: number; prev: unknown } | null>(null);
  const [custom, setCustom] = useState('');
  const set = (k: keyof SpawnForm) => (v: string) => setForm((f) => ({ ...f, [k]: v }));

  const binary = profile === 'custom' ? custom : profile;
  const key = profileKey(binary, form.configDir);
  const blocker = spawnBlocker(s, form.node.trim(), key);
  const idError = form.agentId !== '' && !AGENT_ID.test(form.agentId) ? 'Use 1-64 characters: lowercase letters, digits and "-".' : null;
  const error = pending ? s.toasts.find((t) => t.kind === 'error' && t.id >= pending.since)?.text : undefined;
  const repoSource = source === 'repo';
  const toQueue = !s.nodes.some((n) => (form.node === '' || n.id === form.node.trim()) && n.state === 'Idle' && !(key && blockersFor(s, n.id, key).length > 0));
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
    if (store.intent({ kind: 'spawn', form: { ...form, binary: binary.trim(), repo: repoSource && cred && choice ? { credential: cred.id, ...choice, allowPush } : undefined } })) setPending({ id: form.agentId.trim(), since: store.state.peek().nextToastId, prev: s.statuses[form.agentId.trim()] });
  };
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
            </>
          )}
        </div>
      )}
      <div class="field">
        <span class="label">Profile</span>
        <Segmented<Profile> label="Profile" value={profile} options={PROFILES} onChange={setProfile} />
      </div>
      {profile === 'custom' && <TextField label="Binary" value={custom} placeholder="/usr/local/bin/my-agent" mono onValue={setCustom} />}
      <TextField label="CLAUDE_CONFIG_DIR (optional)" value={form.configDir} placeholder="~/.claude-work" mono onValue={set('configDir')} />
      {!repoSource && <TextField label="Working directory" value={form.workdir} placeholder="/home/me/project" mono onValue={set('workdir')} />}
      <TextField label="Agent id" value={form.agentId} placeholder="worker-1" mono error={idError} autocapitalize="off" spellcheck={false} onValue={set('agentId')} />
      <div class="field">
        <label for="spawn-prompt">Prompt</label>
        <textarea id="spawn-prompt" class="input" rows={3} placeholder="What should the agent do?" value={form.prompt} onInput={(e) => set('prompt')(e.currentTarget.value)} />
      </div>
      <div class="field" role="group" aria-label="Target node">
        <span class="label">Run on</span>
        <div class="chips">
          {[{ id: '', label: 'Auto' }, ...s.nodes.map((n) => ({ id: n.id, label: `${n.hostname}${targetNote(s, n, key)}` }))].map((t) => (
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
