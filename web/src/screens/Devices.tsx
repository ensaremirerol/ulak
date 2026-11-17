import { useEffect, useRef, useState } from 'preact/hooks';
import type { DeviceKind } from '../proto';
import { store } from '../state';
import { Button, Card, EmptyState, Pill } from '../ui/components';
import { ConfirmButton } from '../ui/ConfirmButton';
import { day } from '../ui/format';
import { Segmented } from '../ui/Segmented';

type Choice = 'device' | 'node' | 'ui' | 'browser';
const CHOICES = (['device', 'node', 'ui', 'browser'] as const).map((value) => ({ value, label: value }));
/** "device" is a node on the wire (the same code works for `ulakd join` and `ulak join`). */
export const kindOf = (c: Choice): DeviceKind => (c === 'device' ? 'node' : c);

export const countdown = (ms: number): string => {
  if (ms <= 0) return 'Expired';
  const s = Math.ceil(ms / 1000);
  return `Expires in ${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
};

function OneTimeCode({ choice }: { choice: Choice }) {
  const s = store.state.value;
  const o = s.otp!;
  const code = useRef<HTMLDivElement>(null);
  const left = o.expires_ms - s.now;
  const url = `https://${location.host}`;
  const cmds = [choice !== 'ui' && choice !== 'browser' && `ulakd join ${url}`, (choice === 'ui' || choice === 'device') && `ulak join ${url}`].filter((c): c is string => c !== false);
  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      store.toast('ok', 'Copied.');
    } catch {
      const sel = getSelection();
      if (code.current && sel) {
        sel.removeAllRanges();
        const r = document.createRange();
        r.selectNodeContents(code.current);
        sel.addRange(r);
      }
      store.toast('info', 'Copy is not available here: the code is selected, press Ctrl+C.');
    }
  };
  return (
    <Card title="One-time code" aside={<Pill tone={left <= 0 ? 'bad' : 'warn'}>{countdown(left)}</Pill>}>
      <span class="muted small">Enter this on the new {o.kind}. It works once and expires.</span>
      <div class="otp" ref={code} aria-label="One-time code">{o.otp}</div>
      <div><Button size="sm" variant="ghost" disabled={left <= 0} onClick={() => copy(o.otp)}>Copy code</Button></div>
      <div class="stack tight"><span class="muted small">Hub fingerprint</span><span class="mono small wrap">{o.hub_fingerprint}</span></div>
      <div class="stack tight">
        <span class="muted small">Join command</span>
        {cmds.map((c) => <div key={c} class="row nowrap"><code class="mono small wrap grow">{c}</code><Button size="sm" variant="ghost" onClick={() => copy(c)}>Copy</Button></div>)}
      </div>
    </Card>
  );
}

export function Devices() {
  const s = store.state.value;
  const [choice, setChoice] = useState<Choice>('device');
  const open = s.conn === 'open';
  useEffect(() => {
    if (open) store.intent({ kind: 'refresh_devices' });
  }, [open]);
  return (
    <>
      <header class="page-head"><h1>Devices</h1></header>
      <div class="two-col">
        <Card title="Devices" aside={<Button size="sm" variant="ghost" onClick={() => store.intent({ kind: 'refresh_devices' })}>Refresh</Button>}>
          {s.devices.length === 0 ? <EmptyState title="No devices">Add one with the form on the right.</EmptyState> : (
            <ul class="plain">
              {s.devices.map((d) => (
                <li key={d.id} class="device">
                  <span class="agent-name"><span class="strong">{d.hostname || d.id}</span><span class="muted small">{d.kind} - enrolled {day(d.created_ms)}</span></span>
                  {d.revoked ? <Pill tone="bad">Revoked</Pill> : <ConfirmButton label="Revoke" confirmLabel="Click again to revoke" onConfirm={() => store.intent({ kind: 'revoke_device', id: d.id })} />}
                </li>
              ))}
            </ul>
          )}
        </Card>
        <div class="stack">
          <Card title="Add device">
            <Segmented<Choice> label="Kind" value={choice} options={CHOICES} onChange={setChoice} />
            <div><Button variant="primary" onClick={() => store.intent({ kind: 'add_device', device: kindOf(choice) })}>Create one-time code</Button></div>
          </Card>
          {s.otp && <OneTimeCode choice={choice} />}
        </div>
      </div>
    </>
  );
}
