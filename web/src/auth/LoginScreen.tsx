import { useState } from 'preact/hooks';
import { Button, Card, TextField } from '../ui/components';
import { Segmented } from '../ui/Segmented';
import { registerDevice, signIn } from './api';

type Mode = 'signin' | 'register';

/** The only screen without a session. `onDone` runs after the hub set the cookie (reconnect then). */
export function LoginScreen({ onDone }: { onDone(): void }) {
  const [mode, setMode] = useState<Mode>('signin');
  const [otp, setOtp] = useState('');
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = async (job: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await job();
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <main class="login">
      <Card title="Sign in to ulak">
        <Segmented<Mode> label="Method" value={mode} onChange={setMode} options={[{ value: 'signin', label: 'Sign in' }, { value: 'register', label: 'Register this device' }]} />
        {mode === 'signin' ? (
          <Button variant="primary" disabled={busy} onClick={() => run(() => signIn())}>{busy ? 'Waiting for passkey...' : 'Sign in with passkey'}</Button>
        ) : (
          <form class="stack" onSubmit={(e) => { e.preventDefault(); void run(() => registerDevice(otp, name)); }}>
            <TextField label="One-time code" mono autocomplete="one-time-code" autocapitalize="characters" spellcheck={false} value={otp} onValue={setOtp} hint="From the Devices screen of a signed-in console, or ulak device add." />
            <TextField label="Device name" value={name} onValue={setName} placeholder="this browser" />
            <Button variant="primary" type="submit" disabled={busy}>{busy ? 'Working...' : 'Register and sign in'}</Button>
          </form>
        )}
        {error && <p role="alert" class="muted">{error}</p>}
      </Card>
    </main>
  );
}
