import { useState } from 'preact/hooks';
import { store } from '../state';
import { Button, Card } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { day } from '../ui/format';
import { routeSub } from '../ui/router';
import { AddCredential, removeCredential } from './Credentials';
import { Devices } from './Devices';
import { HubProfiles } from './HubProfiles';
import { Routing } from './Routing';
import { Templates } from './Templates';

const TABS = [['general', 'General'], ['github', 'GitHub'], ['templates', 'Templates'], ['routing', 'Issue routing'], ['devices', 'Devices']] as const;
type Tab = (typeof TABS)[number][0];

function General() {
  const s = store.state.value;
  return (
    <>
      <Card title="Hub">{s.hubVersion ? `${s.hubVersion.version} (${s.hubVersion.git})` : 'unknown'}</Card>
      <Card title="Profiles">
        <p class="muted small">Agent profiles the hub sends to every node. A node profile with the same name wins on that node; set those on the node's page.</p>
        <HubProfiles node={null} label="Global profiles" />
      </Card>
      <Card title="Messages">
        <p class="muted small">Every message agents sent each other. <a href="#/mailbox">Open the message log</a></p>
      </Card>
    </>
  );
}

function GitHub() {
  const s = store.state.value;
  const [adding, setAdding] = useState(false);
  const is = s.issueSettings;
  const issueCred = is?.credential ?? s.credentials[0]?.id ?? '';
  return (
    <>
      <Card title="Credentials" aside={!adding && <Button size="sm" onClick={() => setAdding(true)}>Add</Button>}>
        {s.credentials.length === 0 && !adding && <p class="muted">No GitHub token yet. Add one to start agents from a repository and to see issues.</p>}
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
      {is && s.credentials.length > 1 && (
        <Card title="Issues">
          <div class="field">
            <label for="issue-cred">Read issues, comment and auto-start with</label>
            <select id="issue-cred" class="input" value={issueCred} onChange={(e) => store.intent({ kind: 'save_issue_settings', settings: { ...is, credential: e.currentTarget.value } })}>
              {s.credentials.map((c) => <option key={c.id} value={c.id}>{c.name} ({c.login})</option>)}
            </select>
          </div>
        </Card>
      )}
      {is && (
        <Card title="Tagged repositories">
          <p class="muted small">Repositories that are not yours whose issues show up in Issues. Agents for them start only by hand. Tag more from the Issues screen.</p>
          {is.tagged.length === 0 ? <p class="muted small">None.</p> : (
            <ul class="agent-list" aria-label="Tagged repositories">
              {is.tagged.map((r) => (
                <li key={r} class="row">
                  <span class="mono grow">{r}</span>
                  <Button size="sm" variant="ghost" onClick={() => store.intent({ kind: 'save_issue_settings', settings: { ...is, tagged: is.tagged.filter((x) => x !== r) } })}>Untag</Button>
                </li>
              ))}
            </ul>
          )}
        </Card>
      )}
    </>
  );
}

export function Settings() {
  const s = store.state.value;
  const sub = routeSub.value;
  const tab: Tab = TABS.some(([t]) => t === sub) ? (sub as Tab) : 'general';
  return (
    <>
      <header class="page-head"><h1>Settings</h1></header>
      <nav class="tabs settings-tabs" aria-label="Settings">
        {TABS.map(([t, label]) => <a key={t} class="tab" href={`#/settings/${t}`} aria-current={tab === t ? 'page' : undefined}>{label}</a>)}
      </nav>
      {tab === 'general' && <General />}
      {tab === 'github' && <GitHub />}
      {(tab === 'templates' || tab === 'routing') && !s.issueSettings && <p class="muted" role="status">Loading...</p>}
      {tab === 'templates' && s.issueSettings && <Templates settings={s.issueSettings} />}
      {tab === 'routing' && s.issueSettings && <Routing key={JSON.stringify(s.issueSettings.routing)} settings={s.issueSettings} />}
      {tab === 'devices' && <Devices />}
    </>
  );
}
