import { describe, expect, it } from 'vitest';
import { AuthError, b64uDecode, b64uEncode, cleanOtp, loginJson, prepareOptions, registerJson, registerDevice } from '../src/auth';

const CREATION = {
  publicKey: {
    rp: { name: 'ulak', id: 'hub.example' }, user: { id: 'AQIDBA', name: 'op', displayName: 'op' }, challenge: 'q83vEjRW',
    excludeCredentials: [{ type: 'public-key', id: 'CQgH' }, { type: 'public-key', id: 'AAEC' }], timeout: 60000,
  },
};
const REQUEST = { publicKey: { challenge: 'q83vEjRW', rpId: 'hub.example', allowCredentials: [{ type: 'public-key', id: 'CQgH' }] } };
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Rec = any;
const u8 = (b: unknown) => Array.from(new Uint8Array(b as ArrayBuffer));

describe('base64url', () => {
  it('round-trips and accepts padding', () => {
    const d = new Uint8Array([0, 255, 254, 1, 2, 250]);
    const s = b64uEncode(d);
    expect(s).not.toMatch(/[+/=]/);
    expect(Array.from(b64uDecode(s))).toEqual(Array.from(d));
    expect(Array.from(b64uDecode('AQI='))).toEqual([1, 2]);
    expect(() => b64uDecode('a+b')).toThrow(AuthError);
    expect(() => b64uDecode('a')).toThrow(AuthError);
  });
});

describe('prepareOptions', () => {
  it('decodes challenge, user.id and excludeCredentials ids without mutating the input', () => {
    const before = JSON.stringify(CREATION);
    const pk = prepareOptions(CREATION, 'register') as unknown as Rec;
    expect(JSON.stringify(CREATION)).toBe(before);
    expect(u8(pk['user'].id)).toEqual([1, 2, 3, 4]);
    expect(u8(pk['excludeCredentials'][1].id)).toEqual([0, 1, 2]);
    expect(pk['challenge']).toBeInstanceOf(ArrayBuffer);
    expect(pk['rp'].id).toBe('hub.example');
  });
  it('uses allowCredentials for login and rejects junk', () => {
    const pk = prepareOptions(REQUEST, 'login') as unknown as Rec;
    expect(u8(pk['allowCredentials'][0].id)).toEqual([9, 8, 7]);
    expect(() => prepareOptions({}, 'login')).toThrow(AuthError);
    expect(() => prepareOptions({ publicKey: { challenge: 5 } }, 'login')).toThrow(AuthError);
    expect(() => prepareOptions(REQUEST, 'register')).toThrow(/user/);
  });
});

describe('credential JSON', () => {
  const buf = (...n: number[]) => new Uint8Array(n).buffer;
  const base = { id: 'AAEC', rawId: buf(0, 1, 2), clientDataJSON: buf(123, 125) };
  it('register shape', () => {
    expect(registerJson({ ...base, attestationObject: buf(9, 9), transports: ['internal'] })).toEqual({
      id: 'AAEC', rawId: 'AAEC', type: 'public-key', extensions: {},
      response: { attestationObject: 'CQk', clientDataJSON: 'e30', transports: ['internal'] },
    });
    expect(registerJson({ ...base, attestationObject: buf(9), transports: [] })).not.toHaveProperty('response.transports');
  });
  it('login shape, userHandle only when present', () => {
    const j = loginJson({ ...base, authenticatorData: buf(1, 1), signature: buf(7, 7), userHandle: buf(1, 2, 3, 4) }) as Rec;
    expect(j.response).toEqual({ authenticatorData: 'AQE', clientDataJSON: 'e30', signature: 'Bwc', userHandle: 'AQIDBA' });
    expect(loginJson({ ...base, userHandle: buf() }) as Rec).not.toHaveProperty('response.userHandle');
  });
});

describe('cleanOtp', () => {
  it('strips whitespace and dashes, uppercases', () => {
    expect(cleanOtp(' abcd 2345\n')).toBe('ABCD2345');
    expect(cleanOtp('abcd-2345')).toBe('ABCD2345');
  });
});

describe('registerDevice', () => {
  it('refuses an empty code before any request', async () => {
    let called = false;
    await expect(registerDevice('  ', 'x', (async () => { called = true; return new Response(); }) as typeof fetch)).rejects.toThrow(/one-time code/);
    expect(called).toBe(false);
  });
  it('surfaces the hub error text', async () => {
    const f = (async () => new Response('bad otp', { status: 403 })) as typeof fetch;
    await expect(registerDevice('ABCD', 'x', f)).rejects.toThrow('bad otp');
  });
});
