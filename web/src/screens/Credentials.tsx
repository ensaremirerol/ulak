import { useState } from 'preact/hooks';
import type { CredentialInfo } from '../proto';
import { store } from '../state';
import { Button, TextField } from '../ui/components';

export const TOKEN_URL = 'https://github.com/settings/personal-access-tokens/new';

/** Write-only: the token lives in this component until it is sent, then the field is cleared. The hub stores it. */
export function AddCredential({ onCancel }: { onCancel?(): void }) {
  const [name, setName] = useState('');
  const [token, setToken] = useState('');
  const submit = () => {
    if (store.intent({ kind: 'add_credential', name, token })) {
      setToken('');
      setName('');
      onCancel?.();
    }
  };
  return (
    <form class="stack tight" aria-label="Add a GitHub token" onSubmit={(e) => { e.preventDefault(); submit(); }}>
      <strong>Add a GitHub token</strong>
      <TextField label="Token name" value={name} placeholder="work" autocapitalize="off" onValue={setName} />
      <div class="field">
        <label for="cred-token">GitHub token</label>
        <input id="cred-token" class="input mono" type="password" autocomplete="off" spellcheck={false} value={token} onInput={(e) => setToken(e.currentTarget.value)} />
        <span class="hint">Shown once, then only the hub keeps it.</span>
      </div>
      <a href={TOKEN_URL} target="_blank" rel="noopener noreferrer">Create a fine-grained token with Contents: read (and write if you want agents to push)</a>
      <div class="row">
        <Button type="submit" variant="primary" size="sm">Add token</Button>
        {onCancel && <Button type="button" variant="ghost" size="sm" onClick={onCancel}>Cancel</Button>}
      </div>
    </form>
  );
}

export const removeCredential = (c: CredentialInfo) => store.intent({ kind: 'delete_credential', id: c.id });
