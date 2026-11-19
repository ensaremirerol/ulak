import { useEffect, useRef, useState } from 'preact/hooks';
import type { DeviceKind } from '../proto';
import { store } from '../state';
import { Button, Card, EmptyState, Pill, TextField } from '../ui/components';
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

/** POSIX single-quote: safe for any value, even one containing `'`. */
export const shq = (v: string): string => `'${v.replace(/'/g, `'\\''`)}'`;

/** The hub's origin when `raw` is a bare https:// address, else null. */
export function hubOrigin(raw: string): string | null {
  try {
    const u = new URL(raw.trim());
    return u.protocol === 'https:' && u.pathname === '/' && !u.search && !u.hash && !u.username && !u.password ? u.origin : null;
  } catch {
    return null;
  }
}

const RELEASES = 'https://github.com/ensaremirerol/ulak/releases/latest/download/install-ulakd.sh';

export function installCommands(otp: string, fp: string, hub: string, version: string | null): { linux: string; docker: string } {
  // Inside the already single-quoted -c script a plain origin needs no quotes; anything else nests them.
  const inner = /^[\w.:/-]+$/.test(hub) ? hub : shq(hub);
  const v = version?.replace(/^v/, '');
  const tag = v && /^[\w.-]+$/.test(v) ? v : 'latest';
  return {
    linux: `curl -fsSL ${RELEASES} | ULAKD_OTP=${shq(otp)} ULAKD_FINGERPRINT=${shq(fp)} sh -s -- ${shq(hub)}`,
    docker: `docker run -d --name ulakd --restart always -v ulakd-data:/data -e ULAKD_OTP=${shq(otp)} -e ULAKD_FINGERPRINT=${shq(fp)} --entrypoint sh ghcr.io/ensaremirerol/ulakd:${tag} -c ${shq(`[ -f /data/cert.pem ] || ulakd join ${inner}; exec ulakd run`)}`,
  };
}

function CommandBlock({ title, note, cmd }: { title: string; note: string; cmd: string }) {
  const [status, setStatus] = useState('');
  useEffect(() => {
    if (status !== 'Copied') return;
    const t = setTimeout(() => setStatus(''), 2000);
    return () => clearTimeout(t);
  }, [status]);
  const copy = async () => {
    try {
      if (!navigator.clipboard?.writeText) throw new Error('no clipboard');
      await navigator.clipboard.writeText(cmd);
      setStatus('Copied');
    } catch {
      setStatus('Select the text and copy it');
    }
  };
  return (
    <div class="stack tight">
      <div class="row"><span class="strong small grow">{title}</span><Button size="sm" variant="ghost" onClick={() => void copy()}>Copy</Button></div>
      <pre class="mono small cmd">{cmd}</pre>
      <span class="muted small" aria-live="polite">{status}</span>
      <span class="muted small">{note}</span>
    </div>
  );
}

function NodeInstall({ otp, fp }: { otp: string; fp: string }) {
  const [raw, setRaw] = useState(`https://${location.host}`);
  const hub = hubOrigin(raw);
  const v = store.state.value.hubVersion?.version ?? null;
  const c = hub && installCommands(otp, fp, hub, v);
  return (
    <div class="stack tight">
      <TextField label="Hub address for nodes" mono spellcheck={false} value={raw} onValue={setRaw}
        error={hub ? undefined : 'Use an https:// address with no path or query.'}
        hint="The https:// address nodes connect to (the hub's port, e.g. https://hub.example.org or https://hub:8443)." />
      {c && <>
        <span class="muted small">The code works once and expires; the command includes it.</span>
        <CommandBlock title="Linux or WSL (as your user, no sudo)" cmd={c.linux} note="x86_64 or arm64, glibc 2.34+, needs tmux. WSL: enable systemd first (/etc/wsl.conf: [boot] systemd=true)." />
        <CommandBlock title="Docker" cmd={c.docker} note="Agents run inside the container." />
      </>}
    </div>
  );
}

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
      {o.kind === 'node' && <NodeInstall otp={o.otp} fp={o.hub_fingerprint} />}
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
