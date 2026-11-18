import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiToHub } from '../src/proto';
import { linePath, NodeDetail } from '../src/screens/NodeDetail';
import { Settings } from '../src/screens/Settings';
import { ACK_TIMEOUT_MS, store } from '../src/state';
import { feed, freshStore, node } from './helpers';

const tel = (at_ms: number, cpu_pct: number, gpu_pct: number | null = null) => ({ load1: 0.5, cpu_pct, gpu_pct, mem_used_pct: 40, at_ms });
const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });
let sent: UiToHub[];
type Scoped = Extract<UiToHub, { type: 'save_profile' }>;
/** The hub applied the last save: it broadcasts the new config. */
const applied = (profiles: { node: string | null; profile: Scoped['profile'] }[] = [], settings: { node: string; settings: object }[] = []) =>
  act(() => feed({ type: 'hub_config', profiles, settings }));
afterEach(() => vi.useRealTimers());

beforeEach(() => {
  sent = freshStore();
  feed({ type: 'node_update', ...node('n1', 'alpha', ['w-1']) }, { type: 'agent_status', agent: 'w-1', node: 'n1', status: 'Running' });
});

describe('NodeDetail', () => {
  it('asks for the history, charts it, appends live telemetry, stops on leave', () => {
    const { unmount } = render(<NodeDetail id="n1" />);
    expect(sent).toEqual([{ type: 'node_stats', node: 'n1' }]);
    expect(screen.getByRole('heading', { name: 'alpha' })).toBeTruthy();
    expect(screen.getByText('Collecting history...')).toBeTruthy();
    act(() => feed({ type: 'node_stats', node: 'n1', connected_ms: 1_000, samples: [tel(60_000, 10, 5), tel(120_000, 30, 5)] }));
    expect(screen.getByRole('img').getAttribute('aria-label')).toBe('Last 1 min: CPU now 30%, peak 30%; GPU now 5%, peak 5%; Memory now 40%, peak 40%');
    expect(screen.getByText(/connected since/)).toBeTruthy();
    act(() => feed({ type: 'node_update', ...node('n1', 'alpha', ['w-1']), telemetry: tel(180_000, 80) }));
    expect(store.state.value.stats!.samples).toHaveLength(3);
    expect(screen.getByRole('img').getAttribute('aria-label')).toMatch(/^Last 2 min: CPU now 80%, peak 80%/);
    expect(within(screen.getByRole('list', { name: 'Agents on this node' })).getByText('Running')).toBeTruthy();
    act(() => unmount());
    expect(store.state.value.stats).toBeNull();
  });

  it('switching nodes keeps the new node watched even if the old cleanup runs late', () => {
    feed({ type: 'node_update', ...node('n2', 'beta') });
    const { rerender } = render(<NodeDetail key="n1" id="n1" />);
    act(() => rerender(<NodeDetail key="n2" id="n2" />));
    expect(store.state.value.stats?.node).toBe('n2');
    act(() => { store.intent({ kind: 'unwatch_node', node: 'n1' }); });
    act(() => feed({ type: 'node_stats', node: 'n2', connected_ms: null, samples: [tel(1, 1)] }));
    expect(store.state.value.stats).toEqual({ node: 'n2', connectedMs: null, samples: [tel(1, 1)] });
  });

  it('line path breaks where a value is missing', () => {
    expect(linePath([tel(0, 0, 0), tel(1, 0, null), tel(2, 0, 100)], (t) => t.gpu_pct)).toBe('M0.0 120.0M600.0 0.0');
  });

  it('unknown node shows an empty state', () => {
    render(<NodeDetail id="nope" />);
    expect(screen.getByText('Unknown node')).toBeTruthy();
  });

  it('reported profiles show the hub badge; node profiles add with masked env', () => {
    feed({ type: 'node_profiles', node: 'n1', profiles: [{ name: 'cw', binary: 'claude', config_dir: null, installed: true, login_blocked: false, from_hub: true }] });
    render(<NodeDetail id="n1" />);
    expect(within(screen.getByRole('list', { name: 'Profiles on alpha' })).getByText('from hub')).toBeTruthy();
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Add profile' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.getByText(/Use 1-64 characters/)).toBeTruthy();
    expect(sent).toEqual([]);
    type('Name', 'solo');
    type('Program', 'claude');
    type('Environment', 'TOKEN=s3cret');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toEqual([{ type: 'save_profile', node: 'n1', profile: { name: 'solo', binary: 'claude', env: { TOKEN: 's3cret' } } }]);
    // Open until the hub answers; a refusal keeps the input.
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Saving…' }) as HTMLButtonElement).disabled).toBe(true);
    act(() => feed({ type: 'error', message: 'too many hub profiles' }));
    expect(screen.getByText('Not saved: too many hub profiles')).toBeTruthy();
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('solo');
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toHaveLength(1);
    applied([{ node: 'n1', profile: { name: 'solo', binary: 'claude', env: { TOKEN: 's3cret' } } }]);
    expect(screen.queryByRole('dialog')).toBeNull();
    const list = within(screen.getByRole('list', { name: 'Hub profiles of alpha' }));
    expect(list.getByText('TOKEN=••••••')).toBeTruthy();
    expect(screen.queryByText(/s3cret/)).toBeNull();
    // Edit keeps the hidden value unless removed; the name is fixed.
    fireEvent.click(list.getByRole('button', { name: 'Edit solo' }));
    expect((screen.getByLabelText('Name') as HTMLInputElement).disabled).toBe(true);
    expect(screen.queryByText(/s3cret/)).toBeNull();
    type('Arguments', '--x');
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toEqual([{ type: 'save_profile', node: 'n1', profile: { name: 'solo', binary: 'claude', args: ['--x'], env: { TOKEN: 's3cret' } } }]);
    applied([{ node: 'n1', profile: { name: 'solo', binary: 'claude', args: ['--x'], env: { TOKEN: 's3cret' } } }]);
    fireEvent.click(list.getByRole('button', { name: 'Edit solo' }));
    fireEvent.click(screen.getByRole('button', { name: 'Remove TOKEN' }));
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toEqual([{ type: 'save_profile', node: 'n1', profile: { name: 'solo', binary: 'claude', args: ['--x'] } }]);
    applied([{ node: 'n1', profile: { name: 'solo', binary: 'claude', args: ['--x'] } }]);
    sent.length = 0;
    fireEvent.click(list.getByRole('button', { name: 'Delete' }));
    fireEvent.click(list.getByRole('button', { name: 'Click again to delete' }));
    expect(sent).toEqual([{ type: 'delete_profile', node: 'n1', name: 'solo' }]);
  });

  it('settings form: inline range errors, save, reset to node defaults', () => {
    feed({ type: 'hub_config', profiles: [], settings: [{ node: 'n1', settings: { index_interval_mins: 30, clean_auto: true } }] });
    render(<NodeDetail id="n1" />);
    sent.length = 0;
    expect((screen.getByLabelText('Index every (minutes)') as HTMLInputElement).value).toBe('30');
    type('Busy above CPU %', '150');
    expect(screen.getByText('A percentage, 1-100.')).toBeTruthy();
    expect((screen.getByRole('button', { name: 'Save settings' }) as HTMLButtonElement).disabled).toBe(true);
    type('Busy above CPU %', '');
    fireEvent.click(within(screen.getByRole('radiogroup', { name: 'Install updates automatically' })).getByRole('radio', { name: 'Off' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save settings' }));
    expect(sent).toEqual([{ type: 'save_node_settings', node: 'n1', settings: { index_interval_mins: 30, clean_auto: true, auto_update: false } }]);
    expect(screen.getByRole('button', { name: 'Saving…' })).toBeTruthy();
    act(() => feed({ type: 'error', message: 'node n1 is not enrolled' }));
    expect(screen.getByRole('status').textContent).toBe('Not saved: node n1 is not enrolled');
    fireEvent.click(screen.getByRole('button', { name: 'Save settings' }));
    applied([], [{ node: 'n1', settings: { index_interval_mins: 30, clean_auto: true, auto_update: false } }]);
    expect(screen.getByRole('status').textContent).toBe('Saved; the node applies it now.');
    type('Index every (minutes)', '45');
    expect(screen.getByRole('status').textContent).toBe('');
    type('Index every (minutes)', '30');
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Reset to node defaults' }));
    fireEvent.click(screen.getByRole('button', { name: 'Clears the hub settings: confirm' }));
    expect(sent).toEqual([{ type: 'save_node_settings', node: 'n1', settings: {} }]);
  });

  it('first save on a node without hub settings and the reset both report Saved', () => {
    render(<NodeDetail id="n1" />);
    type('Index every (minutes)', '5');
    fireEvent.click(screen.getByRole('button', { name: 'Save settings' }));
    applied([], [{ node: 'n1', settings: { index_interval_mins: 5 } }]);
    expect(screen.getByRole('status').textContent).toBe('Saved; the node applies it now.');
    expect((screen.getByLabelText('Index every (minutes)') as HTMLInputElement).value).toBe('5');
    fireEvent.click(screen.getByRole('button', { name: 'Reset to node defaults' }));
    fireEvent.click(screen.getByRole('button', { name: 'Clears the hub settings: confirm' }));
    applied();
    expect(screen.getByRole('status').textContent).toBe('Saved; the node applies it now.');
    expect((screen.getByLabelText('Index every (minutes)') as HTMLInputElement).value).toBe('');
  });

  it('settings arriving after the screen opened fill an untouched form, not an edited one', () => {
    render(<NodeDetail id="n1" />);
    applied([], [{ node: 'n1', settings: { cpu_busy_pct: 80 } }]);
    expect((screen.getByLabelText('Busy above CPU %') as HTMLInputElement).value).toBe('80');
    type('Busy above GPU %', '70');
    applied([], [{ node: 'n1', settings: { cpu_busy_pct: 90 } }]);
    expect((screen.getByLabelText('Busy above GPU %') as HTMLInputElement).value).toBe('70');
  });

  it('a save with no answer re-enables the form after the timeout', () => {
    vi.useFakeTimers();
    render(<NodeDetail id="n1" />);
    type('Index every (minutes)', '20');
    fireEvent.click(screen.getByRole('button', { name: 'Save settings' }));
    expect(screen.getByRole('button', { name: 'Saving…' })).toBeTruthy();
    act(() => { vi.advanceTimersByTime(ACK_TIMEOUT_MS); });
    expect(screen.getByRole('status').textContent).toMatch(/^Not saved: No answer from the hub yet/);
    expect((screen.getByRole('button', { name: 'Save settings' }) as HTMLButtonElement).disabled).toBe(false);
  });
});

describe('Settings: global profiles', () => {
  it('lists global profiles only and adds one with node null', () => {
    feed({ type: 'hub_config', profiles: [{ node: null, profile: { name: 'g', binary: 'claude', config_dir: '~/.g' } }, { node: 'n1', profile: { name: 'local', binary: 'x' } }], settings: [] });
    render(<Settings />);
    const list = within(screen.getByRole('list', { name: 'Global profiles' }));
    expect(list.getByText('g')).toBeTruthy();
    expect(list.getByText('config ~/.g')).toBeTruthy();
    expect(screen.queryByText('local')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Add profile' }));
    type('Name', 'h');
    type('Program', 'claude');
    type('Environment', 'ULAK_X=1');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.getByText(/reserved/)).toBeTruthy();
    expect(sent).toEqual([]);
    type('Environment', '');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toEqual([{ type: 'save_profile', node: null, profile: { name: 'h', binary: 'claude' } }]);
  });

  it('install script: sent when set, shown in the list, installs on all nodes after two clicks', () => {
    const script = 'curl -fsSL https://example.org/install.sh | bash';
    render(<Settings />);
    fireEvent.click(screen.getByRole('button', { name: 'Add profile' }));
    type('Name', 'h');
    type('Program', 'claude');
    type('Install script', script);
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toEqual([{ type: 'save_profile', node: null, profile: { name: 'h', binary: 'claude', install: script } }]);
    applied([{ node: null, profile: { name: 'h', binary: 'claude', install: script } }]);
    const list = within(screen.getByRole('list', { name: 'Global profiles' }));
    expect(list.getByText(`install script: ${script}`)).toBeTruthy();
    // Editing shows the script; clearing it leaves the field out.
    fireEvent.click(list.getByRole('button', { name: 'Edit h' }));
    expect((screen.getByLabelText('Install script') as HTMLTextAreaElement).value).toBe(script);
    type('Install script', '');
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toEqual([{ type: 'save_profile', node: null, profile: { name: 'h', binary: 'claude' } }]);
    applied([{ node: null, profile: { name: 'h', binary: 'claude', install: script } }]);
    sent.length = 0;
    fireEvent.click(list.getByRole('button', { name: 'Install on all nodes' }));
    expect(sent).toEqual([]);
    fireEvent.click(list.getByRole('button', { name: 'Runs on every node: confirm' }));
    expect(sent).toEqual([{ type: 'install_profile', node: null, name: 'h' }]);
  });
});
