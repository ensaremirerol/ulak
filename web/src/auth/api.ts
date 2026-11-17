import { AuthError, cleanOtp, loginJson, prepareOptions, rawCredential, registerJson } from './webauthn';

type Fetch = typeof fetch;

async function post(path: string, body: unknown, f: Fetch): Promise<Record<string, unknown>> {
  let r: Response;
  try {
    r = await f(path, {
      method: 'POST', credentials: 'same-origin', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
    });
  } catch {
    throw new AuthError('Cannot reach the hub. Check your connection and try again.');
  }
  if (!r.ok) throw new AuthError((await r.text().catch(() => '')).trim().slice(0, 200) || `The hub refused (HTTP ${r.status}).`);
  return (await r.json().catch(() => ({}))) as Record<string, unknown>;
}

async function ceremony(which: 'register' | 'login', options: unknown): Promise<PublicKeyCredential> {
  if (!navigator.credentials) throw new AuthError('This browser has no passkey support (needs HTTPS).');
  const publicKey = prepareOptions(options, which);
  try {
    const c = await (which === 'register'
      ? navigator.credentials.create({ publicKey: publicKey as PublicKeyCredentialCreationOptions })
      : navigator.credentials.get({ publicKey: publicKey as PublicKeyCredentialRequestOptions }));
    if (!c) throw new AuthError('No credential returned.');
    return c as PublicKeyCredential;
  } catch (e) {
    if (e instanceof AuthError) throw e;
    throw new AuthError(e instanceof DOMException && e.name === 'NotAllowedError' ? 'Cancelled or timed out.' : String(e instanceof Error ? e.message : e));
  }
}

/** Passkey sign-in. The hub sets the session cookie; we keep nothing (the returned token is dropped). */
export async function signIn(f: Fetch = fetch): Promise<void> {
  const start = await post('/auth/login/start', {}, f);
  const cred = await ceremony('login', start['options']);
  await post('/auth/login/finish', { challenge_id: start['challenge_id'], credential: loginJson(rawCredential(cred)) }, f);
}

/** Spend the one-time code, create a passkey, then sign in with it. */
export async function registerDevice(otp: string, name: string, f: Fetch = fetch): Promise<void> {
  const code = cleanOtp(otp);
  if (!code) throw new AuthError('Enter the one-time code first.');
  const start = await post('/auth/register/start', { otp: code, name: name.trim() || 'this browser' }, f);
  const cred = await ceremony('register', start['options']);
  await post('/auth/register/finish', { challenge_id: start['challenge_id'], credential: registerJson(rawCredential(cred)) }, f);
  await signIn(f);
}

export async function logout(f: Fetch = fetch): Promise<void> {
  await f('/auth/logout', { method: 'POST', credentials: 'same-origin' }).catch(() => undefined);
}
