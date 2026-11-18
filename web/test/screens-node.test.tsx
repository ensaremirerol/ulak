import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { beforeEach, describe, expect, it } from 'vitest';
import type { UiToHub } from '../src/proto';
import { linePath, NodeDetail } from '../src/screens/NodeDetail';
import { Settings } from '../src/screens/Settings';
import { store } from '../src/state';
import { feed, freshStore, node } from './helpers';

const tel = (at_ms: number, cpu_pct: number, gpu_pct: number | null = null) => ({ load1: 0.5, cpu_pct, gpu_pct, mem_used_pct: 40, at_ms });
const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });
let sent: UiToHub[];

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
    expect(screen.queryByRole('dialog')).toBeNull();
    act(() => feed({ type: 'hub_config', profiles: [{ node: 'n1', profile: { name: 'solo', binary: 'claude', env: { TOKEN: 's3cret' } } }], settings: [] }));
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
    fireEvent.click(list.getByRole('button', { name: 'Edit solo' }));
    fireEvent.click(screen.getByRole('button', { name: 'Remove TOKEN' }));
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(sent).toEqual([{ type: 'save_profile', node: 'n1', profile: { name: 'solo', binary: 'claude' } }]);
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
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Reset to node defaults' }));
    fireEvent.click(screen.getByRole('button', { name: 'Clears the hub settings: confirm' }));
    expect(sent).toEqual([{ type: 'save_node_settings', node: 'n1', settings: {} }]);
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
});
