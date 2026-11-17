import { store } from '../state';
import { useState } from 'preact/hooks';
import { Button, Card } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { day } from '../ui/format';
import { AddCredential, removeCredential } from './Credentials';

export function Settings() {
  const s = store.state.value;
  const [adding, setAdding] = useState(false);
  return (
    <>
      <header class="page-head"><h1>Settings</h1></header>
      <Card title="Hub">{s.hubVersion ? `${s.hubVersion.version} (${s.hubVersion.git})` : 'unknown'}</Card>
      <Card title="Credentials" aside={!adding && <Button size="sm" onClick={() => setAdding(true)}>Add</Button>}>
        {s.credentials.length === 0 && !adding && <p class="muted">No GitHub token yet. Add one to start agents from a repository.</p>}
        <ul class="agent-list" aria-label="Credentials">
          {s.credentials.map((c) => (
            <li key={c.id} class="row">
              <span class="agent-name"><span>{c.name}</span><span class="muted small">GitHub · {c.login} · added {day(c.created_ms)}</span></span>
              <ConfirmButton label="Delete" confirmLabel="Click again to delete" onConfirm={() => removeCredential(c)} />
            </li>
          ))}
        </ul>
        {adding && <AddCredential onCancel={() => setAdding(false)} />}
      </Card>
    </>
  );
}
