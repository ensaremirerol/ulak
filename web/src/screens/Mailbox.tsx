import { useEffect, useState } from 'preact/hooks';
import { store } from '../state';
import { Button, EmptyState, Pill } from '../ui/components';
import { clock } from '../ui/format';

const PREVIEW = 120;

export function Mailbox() {
  const s = store.state.value;
  const [thread, setThread] = useState<string | null>(null);
  const open = s.conn === 'open';
  useEffect(() => {
    if (open) store.intent({ kind: 'refresh_mailbox', ...(thread ? { thread } : {}) });
  }, [open, thread]);
  const rows = s.mailbox.filter((m) => !thread || m.thread === thread).sort((a, b) => b.at_ms - a.at_ms);
  return (
    <>
      <header class="page-head">
        <h1>Mailbox</h1>
        <div class="row">
          {thread && <Button variant="ghost" onClick={() => setThread(null)}>Clear filter</Button>}
          <Button variant="ghost" onClick={() => store.intent({ kind: 'refresh_mailbox', ...(thread ? { thread } : {}) })}>Refresh</Button>
        </div>
      </header>
      {thread && <p class="muted small">Thread <span class="mono">{thread}</span></p>}
      {rows.length === 0 ? <EmptyState title="No messages">{thread ? 'Nothing in this thread.' : 'Agent messages show up here.'}</EmptyState> : (
        <section class="table-wrap" aria-label="Messages">
          <table>
            <thead><tr><th>Time</th><th>From / to</th><th>Thread</th><th>Status</th><th>Message</th></tr></thead>
            <tbody>
              {rows.map((m) => (
                <tr key={m.id}>
                  <td class="mono nowrap">{clock(m.at_ms)}</td>
                  <td class="nowrap">{m.from} {'->'} {m.to}</td>
                  <td><button type="button" class="linkish mono" aria-label={`Filter thread ${m.thread}`} onClick={() => setThread(m.thread)}>{m.thread.slice(0, 8)}</button></td>
                  <td><Pill tone={m.status === 'Read' ? 'ok' : m.status === 'Delivered' ? 'info' : 'warn'}>{m.status}</Pill></td>
                  <td class="preview">{m.payload.length > PREVIEW ? `${m.payload.slice(0, PREVIEW)}...` : m.payload}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}
    </>
  );
}
