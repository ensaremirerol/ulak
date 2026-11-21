import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiToHub } from '../src/proto';
import { AgentWindow, Agents, groupAgents, KEYS, supportsResize } from '../src/screens/Agents';
import { store } from '../src/state';
import { parseTermAgent, termHref } from '../src/ui/router';
import { blocker, feed, freshStore, node, ofType } from './helpers';

vi.mock('../src/ui/Terminal', () => ({
  Terminal: ({ agent, fit }: { agent: string; fit: boolean }) => <div data-testid="term" data-fit={String(fit)}>{agent}</div>,
}));

let sent: UiToHub[];
const bytes = (f: UiToHub | undefined) => (f?.type === 'pane_input' ? Array.from(f.data) : null);
const version = (v: string) => ({ type: 'node_version', node: 'n1', info: { version: v, git: 'g', proto: 1, os: 'linux', arch: 'x86_64' } }) as const;
beforeEach(() => {
  sent = freshStore();
  feed({ type: 'node_update', ...node('n1', 'box', ['a1', 'a2']) }, { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' },
    { type: 'agent_status', agent: 'a2', node: 'n1', status: 'Running' });
});
afterEach(() => vi.unstubAllGlobals());

describe('groupAgents', () => {
  it('puts what needs the user first, with the reason and the action', () => {
    feed(
      { type: 'question_opened', id: 'q', from: 'a1', text: 'keep v1?', at_ms: 1 },
      { type: 'blocker_opened', ...blocker('b1', 'n1', { agent: 'svc', kind: 'Install', detail: 'npm needs root' }) },
      { type: 'agent_status', agent: 'dead', node: 'n1', status: 'Failed' },
      { type: 'agent_failed', agent: 'dead', node: 'n1', reason: 'install failed' },
      { type: 'agent_status', agent: 'old', node: 'n1', status: { Exited: 0 } },
    );
    const g = groupAgents(store.state.value);
    expect(g.needs.map((r) => [r.id, r.kind, r.reason, r.action])).toEqual([
      ['a1', 'Asks you', 'keep v1?', 'answer'],
      ['dead', 'Failed', 'install failed', 'remove'],
      ['svc', 'Install failed', 'npm needs root', 'inbox'],
    ]);
    expect(g.running.map((r) => r.id)).toEqual(['a2']);
    expect(g.ended.map((r) => r.id)).toEqual(['old']);
    expect(groupAgents(store.state.value, 'BOX').running.map((r) => r.id)).toEqual(['a2']);
    expect(groupAgents(store.state.value, 'zzz').running).toEqual([]);
  });

  it('only nodes from 0.1.10 resize the pane', () => {
    expect(['0.1.9', '0.1.10', '0.2.0', '1.0.0', '0.1.10-rc1', undefined, 'x'].map(supportsResize)).toEqual([false, true, true, true, true, false, false]);
  });
});

describe('Agents', () => {
  it('lists running agents, hides ended ones behind a toggle, shows empty states', () => {
    feed({ type: 'agent_status', agent: 'old', node: 'n1', status: { Exited: 3 } });
    const { unmount } = render(<Agents />);
    const list = within(screen.getByRole('list', { name: 'Agents' }));
    expect(list.getByText('a1').closest('button')!.textContent).toContain('Running');
    expect(screen.queryByText('old')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Show 1 ended' }));
    expect(screen.getByText('old').closest('button')!.textContent).toContain('Exited (3)');
    expect(screen.getByText('Select an agent to open its terminal.')).toBeTruthy();
    unmount();
    freshStore();
    render(<Agents />);
    expect(screen.getByText('No nodes connected')).toBeTruthy();
    expect(screen.getByText('No agents yet')).toBeTruthy();
  });

  it('a failed agent can be removed from its Needs you card', () => {
    feed({ type: 'agent_status', agent: 'dead', node: 'n1', status: 'Failed' }, { type: 'agent_failed', agent: 'dead', node: 'n1', reason: 'boom' });
    render(<Agents />);
    const card = within(screen.getByRole('listitem', { name: 'dead: Failed' }));
    expect(card.getByText('boom')).toBeTruthy();
    fireEvent.click(card.getByRole('button', { name: 'Remove' }));
    fireEvent.click(card.getByRole('button', { name: 'Click again to remove' }));
    expect(ofType(sent, 'kill')).toEqual([{ type: 'kill', agent: 'dead' }]);
  });

  it('shows a Starting placeholder instead of the terminal until the agent runs', () => {
    feed({ type: 'agent_status', agent: 'a1', node: 'n1', status: 'Starting' });
    render(<Agents />);
    fireEvent.click(screen.getByText('a1').closest('button')!);
    expect(screen.queryByTestId('term')).toBeNull();
    expect(screen.getByRole('status').textContent).toContain('Starting…');
    expect(ofType(sent, 'attach')).toEqual([]);
    act(() => feed({ type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' }));
    expect(screen.getByTestId('term')).toBeTruthy();
    expect(ofType(sent, 'attach')).toEqual([{ type: 'attach', agent: 'a1' }]);
  });

  it('select attaches, switching detaches the previous first', () => {
    render(<Agents />);
    fireEvent.click(screen.getByText('a1'));
    expect(sent).toEqual([{ type: 'attach', agent: 'a1' }]);
    expect(screen.getByRole('region', { name: 'Terminal of a1' })).toBeTruthy();
    fireEvent.click(screen.getByText('a2'));
    expect(sent.slice(1)).toEqual([{ type: 'detach', agent: 'a1' }, { type: 'attach', agent: 'a2' }]);
  });

  it('fits the terminal only on nodes that can resize the pane', () => {
    render(<Agents />);
    fireEvent.click(screen.getByText('a1'));
    expect(screen.getByTestId('term').dataset.fit).toBe('false');
    act(() => feed(version('0.1.10')));
    expect(screen.getByTestId('term').dataset.fit).toBe('true');
  });

  const open = () => {
    render(<Agents />);
    fireEvent.click(screen.getByText('a1'));
    sent.length = 0;
  };

  it('sending input emits pane_input with the text and a CR, then clears the box', () => {
    open();
    const input = screen.getByLabelText('Send input to a1') as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'hi' } });
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(sent).toHaveLength(1);
    expect(bytes(sent[0])).toEqual([0x68, 0x69, 0x0d]);
    expect(input.value).toBe('');
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(sent).toHaveLength(1);
  });

  it('key bar buttons send their escape sequences', () => {
    open();
    const bar = within(screen.getByLabelText('Keys'));
    for (const [label, seq] of KEYS) {
      sent.length = 0;
      fireEvent.click(bar.getByRole('button', { name: label }));
      expect(bytes(sent[0]), label).toEqual(Array.from(new TextEncoder().encode(seq)));
    }
  });

  it('Close detaches; Stop agent needs two clicks', () => {
    open();
    fireEvent.click(screen.getByRole('button', { name: 'Stop agent' }));
    expect(sent).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Click again to stop' }));
    expect(sent).toEqual([{ type: 'kill', agent: 'a1' }]);
    act(() => { store.intent({ kind: 'select', agent: 'a2' }); });
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(sent).toEqual([{ type: 'detach', agent: 'a2' }]);
    expect(store.state.value.selected).toBeNull();
  });

  it('Messages lists the mail to and from the agent; Details names the node', () => {
    open();
    fireEvent.click(screen.getByRole('tab', { name: 'Messages' }));
    expect(ofType(sent, 'mailbox_log')).toHaveLength(1);
    act(() => feed({ type: 'mailbox_log', messages: [
      { id: 'm1', from: 'a2', to: 'a1', thread: 't', payload: 'hello a1', status: 'Read', at_ms: 1 },
      { id: 'm2', from: 'a2', to: 'x', thread: 't', payload: 'not for a1', status: 'Read', at_ms: 2 },
    ] }));
    expect(screen.getByText('hello a1')).toBeTruthy();
    expect(screen.queryByText('not for a1')).toBeNull();
    fireEvent.click(screen.getByRole('tab', { name: 'Details' }));
    expect(screen.getByRole('link', { name: 'box' })).toBeTruthy();
    expect(screen.getByText(/Fixed 200 x 50/)).toBeTruthy();
  });

  it('New window opens the agent alone, detaches here, and comes back when that window closes', () => {
    vi.useFakeTimers();
    const win = { closed: false, close: vi.fn() };
    const openWin = vi.fn((..._args: unknown[]) => win);
    vi.stubGlobal('open', openWin);
    open();
    fireEvent.click(screen.getByRole('button', { name: 'New window' }));
    expect(openWin.mock.calls[0]![0]).toContain(termHref('a1'));
    expect(sent).toEqual([{ type: 'detach', agent: 'a1' }]);
    expect(screen.getByText(/is open in a separate window/)).toBeTruthy();
    win.closed = true;
    act(() => { vi.advanceTimersByTime(1100); });
    expect(sent.at(-1)).toEqual({ type: 'attach', agent: 'a1' });
    expect(store.state.value.selected).toBe('a1');
    vi.useRealTimers();
  });

  it('a blocked pop-up says so and keeps the terminal here', () => {
    vi.stubGlobal('open', vi.fn(() => null));
    open();
    fireEvent.click(screen.getByRole('button', { name: 'New window' }));
    expect(sent).toEqual([]);
    expect(store.state.value.toasts.at(-1)?.text).toContain('allow pop-ups');
  });
});

describe('separate window', () => {
  it('parses its route and attaches to the agent once connected', () => {
    expect(parseTermAgent('#/term/a%201')).toBe('a 1');
    expect(parseTermAgent('#/agents')).toBeNull();
    expect(parseTermAgent('#/term/%E0')).toBeNull();
    sent = freshStore('connecting');
    feed({ type: 'node_update', ...node('n1', 'box', ['a1']) }, { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' });
    render(<AgentWindow agent="a1" />);
    expect(sent).toEqual([]);
    act(() => store.dispatch({ kind: 'conn', state: 'open' }));
    expect(ofType(sent, 'attach')).toEqual([{ type: 'attach', agent: 'a1' }]);
    expect(screen.queryByRole('button', { name: 'New window' })).toBeNull();
    expect(document.title).toBe('a1 · ulak');
  });
});
