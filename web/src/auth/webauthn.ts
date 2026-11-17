// Pure WebAuthn <-> hub JSON conversions (port of crates/ulak/src/web_login.rs). The hub speaks
// webauthn-rs JSON: base64url strings where the browser API wants ArrayBuffers.

export class AuthError extends Error {}

export function b64uEncode(b: ArrayBuffer | Uint8Array): string {
  const u = b instanceof Uint8Array ? b : new Uint8Array(b);
  let s = '';
  for (const x of u) s += String.fromCharCode(x);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/** Decodes base64url, with or without `=` padding. Throws AuthError on junk. */
export function b64uDecode(s: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Za-z0-9_-]*={0,2}$/.test(s)) throw new AuthError('bad base64url');
  try {
    const bin = atob(s.replace(/=+$/, '').replace(/-/g, '+').replace(/_/g, '/'));
    return Uint8Array.from(bin, (c) => c.charCodeAt(0));
  } catch {
    throw new AuthError('bad base64url');
  }
}

export type Ceremony = 'register' | 'login';
type Json = Record<string, unknown>;

const isObj = (v: unknown): v is Json => typeof v === 'object' && v !== null && !Array.isArray(v);
function bin(o: Json, key: string, what: string): void {
  const v = o[key];
  if (typeof v !== 'string') throw new AuthError(`options has no ${what}`);
  o[key] = b64uDecode(v).buffer;
}

/** Hub `options` -> the `publicKey` argument of navigator.credentials.create/get. Input is not mutated. */
export function prepareOptions(options: unknown, which: Ceremony): PublicKeyCredentialCreationOptions | PublicKeyCredentialRequestOptions {
  if (!isObj(options) || !isObj(options['publicKey'])) throw new AuthError('options has no publicKey');
  const pk: Json = structuredClone(options['publicKey']);
  bin(pk, 'challenge', 'challenge');
  if (which === 'register') {
    if (!isObj(pk['user'])) throw new AuthError('options has no user.id');
    bin(pk['user'], 'id', 'user.id');
  }
  const list = pk[which === 'register' ? 'excludeCredentials' : 'allowCredentials'];
  if (Array.isArray(list)) {
    for (const item of list) {
      if (!isObj(item)) throw new AuthError('bad credential descriptor');
      bin(item, 'id', 'credential descriptor id');
    }
  }
  return pk as unknown as PublicKeyCredentialCreationOptions;
}

/** What the glue reads off a PublicKeyCredential. */
export interface RawCredential {
  id: string;
  rawId: ArrayBuffer;
  clientDataJSON: ArrayBuffer;
  /** Registration only. */
  attestationObject?: ArrayBuffer;
  transports?: string[];
  /** Login only. */
  authenticatorData?: ArrayBuffer;
  signature?: ArrayBuffer;
  userHandle?: ArrayBuffer | null;
}

const empty = new ArrayBuffer(0);

/** `credential` for POST /auth/register/finish. */
export function registerJson(c: RawCredential): unknown {
  return {
    id: c.id, rawId: b64uEncode(c.rawId), type: 'public-key', extensions: {},
    response: {
      attestationObject: b64uEncode(c.attestationObject ?? empty),
      clientDataJSON: b64uEncode(c.clientDataJSON),
      ...(c.transports?.length ? { transports: c.transports } : {}),
    },
  };
}

/** `credential` for POST /auth/login/finish. */
export function loginJson(c: RawCredential): unknown {
  return {
    id: c.id, rawId: b64uEncode(c.rawId), type: 'public-key', extensions: {},
    response: {
      authenticatorData: b64uEncode(c.authenticatorData ?? empty),
      clientDataJSON: b64uEncode(c.clientDataJSON),
      signature: b64uEncode(c.signature ?? empty),
      ...(c.userHandle && c.userHandle.byteLength > 0 ? { userHandle: b64uEncode(c.userHandle) } : {}),
    },
  };
}

/** OTPs are shown as `XXXX XXXX`; the hub wants the bare code. */
export const cleanOtp = (s: string): string => s.replace(/[\s-]/g, '').toUpperCase();

/** Read the fields off a browser PublicKeyCredential. */
export function rawCredential(c: PublicKeyCredential): RawCredential {
  const r = c.response as AuthenticatorAttestationResponse & AuthenticatorAssertionResponse;
  return {
    id: c.id, rawId: c.rawId, clientDataJSON: r.clientDataJSON,
    attestationObject: r.attestationObject, transports: r.getTransports?.(),
    authenticatorData: r.authenticatorData, signature: r.signature, userHandle: r.userHandle,
  };
}
