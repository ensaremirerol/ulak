import { useId, useState } from 'preact/hooks';
import type { HubProfile } from '../proto';
import { emptyProfileForm, profileErrors, profileToForm, store, type ProfileForm } from '../state';
import { Button, TextField } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { Dialog } from '../ui/Dialog';

/** Env values are write-only in the UI: once saved only the keys show. */
export const MASK = '••••••';

/** Add (no `editing`) or edit a hub profile; `node` null = global. Env values of `editing` are kept unless removed. */
export function ProfileDialog({ node, editing, onClose }: { node: string | null; editing?: HubProfile; onClose(): void }) {
  const [form, setForm] = useState<ProfileForm>(() => (editing ? profileToForm(editing) : emptyProfileForm()));
  const [keep, setKeep] = useState<Record<string, string>>(editing?.env ?? {});
  const [tried, setTried] = useState(false);
  const envId = useId();
  const set = (k: keyof ProfileForm) => (v: string) => setForm((f) => ({ ...f, [k]: v }));
  const errors = tried ? profileErrors(form, keep) : {};
  const save = () => {
    setTried(true);
    if (Object.keys(profileErrors(form, keep)).length > 0) return;
    if (store.intent({ kind: 'save_profile', node, form, keepEnv: keep })) onClose();
  };
  return (
    <Dialog
      title={editing ? `Edit profile ${editing.name}` : node ? 'Add a profile for this node' : 'Add a profile'}
      onClose={onClose}
      actions={<><Button variant="ghost" onClick={onClose}>Cancel</Button><Button variant="primary" onClick={save}>Save</Button></>}
    >
      <TextField label="Name" required value={form.name} disabled={!!editing} mono autocapitalize="off" spellcheck={false} placeholder="claude-work" error={errors.name} hint="What a spawn picks; letters, digits, . _ -" onValue={set('name')} />
      <TextField label="Program" required value={form.binary} mono autocapitalize="off" spellcheck={false} placeholder="claude" error={errors.binary} onValue={set('binary')} />
      <TextField label="Arguments" value={form.args} mono autocapitalize="off" spellcheck={false} placeholder="--model opus" error={errors.args} hint="Separated by spaces, placed before the spawn's own." onValue={set('args')} />
      <TextField label="Config folder" value={form.configDir} mono autocapitalize="off" spellcheck={false} placeholder="~/.claude-work" error={errors.configDir} hint="Becomes CLAUDE_CONFIG_DIR for claude." onValue={set('configDir')} />
      {Object.keys(keep).length > 0 && (
        <ul class="repo-list small" aria-label="Saved environment">
          {Object.keys(keep).map((k) => (
            <li key={k}>
              <span class="mono">{k}={MASK}</span>
              <Button size="sm" variant="ghost" aria-label={`Remove ${k}`} onClick={() => setKeep(({ [k]: _gone, ...rest }) => rest)}>Remove</Button>
            </li>
          ))}
        </ul>
      )}
      <div class="field">
        <label for={envId}>{Object.keys(keep).length > 0 ? 'Add or replace environment' : 'Environment'}</label>
        <textarea id={envId} class="input mono" rows={3} placeholder="HTTPS_PROXY=http://proxy:3128" spellcheck={false} value={form.env} aria-invalid={errors.env ? true : undefined} onInput={(e) => set('env')(e.currentTarget.value)} />
        {errors.env ? <span class="err" role="alert">{errors.env}</span> : <span class="hint">One KEY=value per line. Values are hidden after saving.</span>}
      </div>
    </Dialog>
  );
}

/** The hub profiles of one scope (`node` null = global) with Add / Edit / Delete. */
export function HubProfiles({ node, label }: { node: string | null; label: string }) {
  const s = store.state.value;
  const [open, setOpen] = useState<{ editing?: HubProfile } | null>(null);
  const mine = s.hubProfiles.filter((p) => p.node === node).map((p) => p.profile);
  return (
    <div class="stack tight">
      {mine.length === 0 ? <p class="muted small">No hub profiles here yet.</p> : (
        <ul class="agent-list" aria-label={label}>
          {mine.map((p) => (
            <li key={p.name} class="row">
              <span class="agent-name">
                <span class="strong">{p.name}</span>
                <span class="muted small mono wrap">{[p.binary, ...(p.args ?? [])].join(' ')}</span>
                {p.config_dir && <span class="muted small mono wrap">config {p.config_dir}</span>}
                {p.env && <span class="muted small mono wrap">{Object.keys(p.env).map((k) => `${k}=${MASK}`).join(' ')}</span>}
              </span>
              <Button size="sm" variant="ghost" aria-label={`Edit ${p.name}`} onClick={() => setOpen({ editing: p })}>Edit</Button>
              <ConfirmButton label="Delete" confirmLabel="Click again to delete" onConfirm={() => store.intent({ kind: 'delete_profile', node, name: p.name })} />
            </li>
          ))}
        </ul>
      )}
      <div class="row"><Button size="sm" onClick={() => setOpen({})}>Add profile</Button></div>
      {open && <ProfileDialog node={node} editing={open.editing} onClose={() => setOpen(null)} />}
    </div>
  );
}
