// Dev helper: software ES256 WebAuthn authenticator (attestation none) for a dev hub started with
// ULAK_DEV_HTTP=1 ULAK_DEV_HTTP_PORT=18080 (origin http://localhost:18080).
// usage: node web/scripts/dev-login.mjs <browser-otp>  -> registers, logs in, prints a session token.
// Then open http://localhost:18080/#token=<token> and reload once.
import crypto from 'node:crypto';
const BASE = 'http://localhost:18080', ORIGIN = BASE, RP = 'localhost';
const b64u = (b) => Buffer.from(b).toString('base64url');
const sha = (b) => crypto.createHash('sha256').update(b).digest();
const head = (maj, n) => n < 24 ? [maj << 5 | n] : n < 256 ? [maj << 5 | 24, n] : [maj << 5 | 25, n >> 8, n & 255];
const cbor = (v) => {
  if (typeof v === 'number') return Buffer.from(v >= 0 ? head(0, v) : head(1, -1 - v));
  if (typeof v === 'string') return Buffer.concat([Buffer.from(head(3, Buffer.byteLength(v))), Buffer.from(v)]);
  if (Buffer.isBuffer(v)) return Buffer.concat([Buffer.from(head(2, v.length)), v]);
  const e = v instanceof Map ? [...v] : Object.entries(v);
  return Buffer.concat([Buffer.from(head(5, e.length)), ...e.flatMap(([k, x]) => [cbor(k), cbor(x)])]);
};
const post = async (p, body) => {
  const r = await fetch(BASE + p, { method: 'POST', headers: { 'content-type': 'application/json', origin: ORIGIN }, body: JSON.stringify(body) });
  const t = await r.text();
  if (!r.ok) throw new Error(`${p} ${r.status} ${t}`);
  return t ? JSON.parse(t) : {};
};
const { privateKey, publicKey } = crypto.generateKeyPairSync('ec', { namedCurve: 'P-256' });
const jwk = publicKey.export({ format: 'jwk' });
const credId = crypto.randomBytes(32);
const cose = cbor(new Map([[1, 2], [3, -7], [-1, 1], [-2, Buffer.from(jwk.x, 'base64url')], [-3, Buffer.from(jwk.y, 'base64url')]]));
const client = (type, challenge) => Buffer.from(JSON.stringify({ type, challenge, origin: ORIGIN, crossOrigin: false }));

const start = await post('/auth/register/start', { otp: process.argv[2], name: 'soft passkey' });
const cd = client('webauthn.create', start.options.publicKey.challenge);
const authData = Buffer.concat([sha(RP), Buffer.from([0x45]), Buffer.alloc(4), Buffer.alloc(16), Buffer.from([credId.length >> 8, credId.length & 255]), credId, cose]);
await post('/auth/register/finish', { challenge_id: start.challenge_id, credential: {
  id: b64u(credId), rawId: b64u(credId), type: 'public-key', extensions: {},
  response: { attestationObject: b64u(cbor({ fmt: 'none', attStmt: {}, authData })), clientDataJSON: b64u(cd) } } });

const ls = await post('/auth/login/start', {});
const lcd = client('webauthn.get', ls.options.publicKey.challenge);
const ad = Buffer.concat([sha(RP), Buffer.from([0x05]), Buffer.from([0, 0, 0, 1])]);
const sig = crypto.sign('sha256', Buffer.concat([ad, sha(lcd)]), privateKey);
const fin = await post('/auth/login/finish', { challenge_id: ls.challenge_id, credential: {
  id: b64u(credId), rawId: b64u(credId), type: 'public-key', extensions: {},
  response: { authenticatorData: b64u(ad), clientDataJSON: b64u(lcd), signature: b64u(sig) } } });
console.log(fin.token ?? JSON.stringify(fin));
