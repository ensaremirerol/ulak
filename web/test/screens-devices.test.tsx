import { act, fireEvent, render, screen } from '@testing-library/preact';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiToHub } from '../src/proto';
import { countdown, Devices, hubOrigin, installCommands, kindOf, shq } from '../src/screens/Devices';
import { store } from '../src/state';
import { feed, freshStore, ofType } from './helpers';

let sent: UiToHub[];
beforeEach(() => { sent = freshStore(); });
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

const offer = (kind: 'node' | 'ui' | 'browser' = 'node', expires_ms = 1_000_000 + 90_000) =>
  feed({ type: 'otp_offer', kind, otp: 'ABC-123', hub_fingerprint: 'SHA256:deadbeef', expires_ms });

describe('Devices', () => {
  it('asks for the list on open, lists devices, empty state', () => {
    const { unmount } = render(<Devices />);
    expect(ofType(sent, 'list_devices')).toHaveLength(1);
    expect(screen.getByText('No devices')).toBeTruthy();
    unmount();
    feed({ type: 'devices', devices: [
      { id: 'd1', kind: 'node', hostname: 'box', created_ms: 0, revoked: false },
      { id: 'd2', kind: 'ui', hostname: '', created_ms: 0, revoked: true },
    ] });
    render(<Devices />);
    expect(screen.getByText('box')).toBeTruthy();
    expect(screen.getByText('d2')).toBeTruthy();
    expect(screen.getByText('Revoked')).toBeTruthy();
    expect(screen.getAllByRole('button', { name: 'Revoke' })).toHaveLength(1);
  });

  it('does not refresh while disconnected', () => {
    sent = freshStore('connecting');
    render(<Devices />);
    expect(sent).toEqual([]);
  });

  it('kind selection maps to the add_device frame (device -> node)', () => {
    expect(kindOf('device')).toBe('node');
    render(<Devices />);
    sent.length = 0;
    const add = () => fireEvent.click(screen.getByRole('button', { name: 'Create one-time code' }));
    add();
    expect(sent.pop()).toEqual({ type: 'add_device', kind: 'node' });
    for (const k of ['node', 'ui', 'browser'] as const) {
      fireEvent.click(screen.getByRole('radio', { name: k }));
      add();
      expect(sent.pop()).toEqual({ type: 'add_device', kind: k });
    }
  });

  it('shows the OTP, fingerprint and join commands per kind', () => {
    offer();
    const { unmount } = render(<Devices />);
    expect(screen.getByLabelText('One-time code').textContent).toBe('ABC-123');
    expect(screen.getByText('SHA256:deadbeef')).toBeTruthy();
    expect(screen.getByText('Expires in 1:30')).toBeTruthy();
    expect(screen.getByText(/^ulakd join https:/)).toBeTruthy();
    expect(screen.getByText(/^ulak join https:/)).toBeTruthy();
    fireEvent.click(screen.getByRole('radio', { name: 'browser' }));
    expect(screen.queryByText(/^ulakd? join/)).toBeNull();
    fireEvent.click(screen.getByRole('radio', { name: 'node' }));
    expect(screen.getByText(/^ulakd join/)).toBeTruthy();
    expect(screen.queryByText(/^ulak join/)).toBeNull();
    unmount();
  });

  it('countdown formats and clamps at Expired', () => {
    expect(countdown(61_000)).toBe('Expires in 1:01');
    expect(countdown(1)).toBe('Expires in 0:01');
    expect(countdown(0)).toBe('Expired');
    expect(countdown(-5000)).toBe('Expired');
  });

  it('countdown ticks down and clamps at Expired, copy disabled', () => {
    vi.useFakeTimers();
    offer('node', 1_000_000 + 3_000);
    render(<Devices />);
    expect(screen.getByText('Expires in 0:03')).toBeTruthy();
    act(() => store.dispatch({ kind: 'tick', now: 1_000_000 + 2_000 }));
    expect(screen.getByText('Expires in 0:01')).toBeTruthy();
    act(() => store.dispatch({ kind: 'tick', now: 1_000_000 + 60_000 }));
    expect(screen.getByText('Expired')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Copy code' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('revoke needs two clicks', () => {
    feed({ type: 'devices', devices: [{ id: 'd1', kind: 'node', hostname: 'box', created_ms: 0, revoked: false }] });
    render(<Devices />);
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    expect(sent).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Click again to revoke' }));
    expect(sent).toEqual([{ type: 'revoke_device', id: 'd1' }]);
  });

  it('copy uses the clipboard when present', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    offer();
    render(<Devices />);
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'Copy code' })); });
    expect(writeText).toHaveBeenCalledWith('ABC-123');
    expect(store.state.value.toasts.at(-1)).toMatchObject({ kind: 'ok', text: 'Copied.' });
  });

  it('copy falls back to selecting the code when the clipboard fails', async () => {
    vi.stubGlobal('navigator', { clipboard: { writeText: vi.fn().mockRejectedValue(new Error('denied')) } });
    offer();
    render(<Devices />);
    await act(async () => { fireEvent.click(screen.getByRole('button', { name: 'Copy code' })); });
    expect(store.state.value.toasts.at(-1)?.text).toContain('the code is selected');
    expect(getSelection()?.toString()).toBe('ABC-123');
  });

  it('node offers show install blocks with values substituted; browser offers none', () => {
    feed({ type: 'hub_version', version: '0.1.7', git: 'g', proto: 1, os: 'linux', arch: 'x86_64' });
    offer();
    const { unmount } = render(<Devices />);
    const input = screen.getByLabelText('Hub address for nodes') as HTMLInputElement;
    expect(input.value).toBe(`https://${location.host}`);
    const hub = 'https://hub.example.org';
    fireEvent.input(input, { target: { value: hub } });
    const pres = () => [...document.querySelectorAll('pre')].map((p) => p.textContent ?? '');
    const [linux, docker] = pres();
    expect(linux).toBe(`curl -fsSL https://github.com/ensaremirerol/ulak/releases/latest/download/install-ulakd.sh | ULAKD_OTP='ABC-123' ULAKD_FINGERPRINT='SHA256:deadbeef' sh -s -- '${hub}'`);
    expect(docker).toContain("-e ULAKD_OTP='ABC-123' -e ULAKD_FINGERPRINT='SHA256:deadbeef'");
    expect(docker).toContain(`ghcr.io/ensaremirerol/ulakd:0.1.7 -c '[ -f /data/cert.pem ] || ulakd join ${hub}; exec ulakd run'`);
    unmount();
    offer('browser');
    render(<Devices />);
    expect(pres()).toEqual([]);
    expect(screen.queryByLabelText('Hub address for nodes')).toBeNull();
  });

  it('hub address edits update the commands; invalid ones hide the blocks', () => {
    offer();
    render(<Devices />);
    const input = screen.getByLabelText('Hub address for nodes');
    fireEvent.input(input, { target: { value: 'https://hub:8443' } });
    expect(document.querySelector('pre')?.textContent).toContain("sh -s -- 'https://hub:8443'");
    expect(document.querySelectorAll('pre')[1]?.textContent).toContain('ulakd:latest');
    for (const bad of ['http://hub:8443', 'https://hub/x', 'https://hub?a=1', 'nope']) {
      fireEvent.input(input, { target: { value: bad } });
      expect(screen.getByRole('alert').textContent).toContain('https://');
      expect(document.querySelector('pre')).toBeNull();
    }
  });

  it('quotes values for a POSIX shell', () => {
    expect(shq("a'b")).toBe(String.raw`'a'\''b'`);
    expect(hubOrigin('https://hub/')).toBe('https://hub');
    expect(hubOrigin('https://u:p@hub')).toBeNull();
    const c = installCommands("X'Y", 'F', "https://h'", 'v1.2');
    expect(c.linux).toContain(String.raw`ULAKD_OTP='X'\''Y'`);
    expect(c.docker.endsWith(`ulakd:1.2 -c ${shq(`[ -f /data/cert.pem ] || ulakd join ${shq("https://h'")}; exec ulakd run`)}`)).toBe(true);
    expect(shq("''")).toBe(String.raw`''\'''\'''`);
  });

  it('copy shows Copied, or a fallback without a clipboard', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    offer();
    const { unmount } = render(<Devices />);
    fireEvent.input(screen.getByLabelText('Hub address for nodes'), { target: { value: 'https://hub' } });
    await act(async () => { fireEvent.click(screen.getAllByRole('button', { name: 'Copy' }).at(-2)!); });
    expect(writeText.mock.calls[0]?.[0]).toMatch(/^curl /);
    expect(screen.getByText('Copied')).toBeTruthy();
    unmount();
    vi.stubGlobal('navigator', {});
    render(<Devices />);
    fireEvent.input(screen.getByLabelText('Hub address for nodes'), { target: { value: 'https://hub' } });
    await act(async () => { fireEvent.click(screen.getAllByRole('button', { name: 'Copy' }).at(-1)!); });
    expect(screen.getByText('Select the text and copy it')).toBeTruthy();
  });
});
