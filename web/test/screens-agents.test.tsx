import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { UiToHub } from '../src/proto';
import { Agents, KEYS } from '../src/screens/Agents';
import { store } from '../src/state';
import { feed, freshStore, node, ofType } from './helpers';

vi.mock('../src/ui/Terminal', () => ({ Terminal: ({ agent }: { agent: string }) => <div data-testid="term">{agent}</div> }));

let sent: UiToHub[];
const bytes = (f: UiToHub | undefined) => (f?.type === 'pane_input' ? Array.from(f.data) : null);
beforeEach(() => {
  sent = freshStore();
  feed({ type: 'node_update', ...node('n1', 'box', ['a1', 'a2']) }, { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' },
    { type: 'agent_status', agent: 'a2', node: 'n1', status: { Exited: 3 } });
});

describe('Agents', () => {
  it('renders nodes and the agent list with statuses', () => {
    render(<Agents />);
    expect(screen.getByRole('article', { name: 'Node box' }).textContent).toContain('2 agents');
    const list = within(screen.getByRole('list', { name: 'Agents' }));
    expect(list.getByText('a1').closest('button')!.textContent).toContain('Running');
    expect(list.getByText('a2').closest('button')!.textContent).toContain('Exited (3)');
    expect(screen.getByText('Select an agent to open its terminal.')).toBeTruthy();
  });

  it('shows a Starting placeholder instead of the terminal until the agent runs', () => {
    feed({ type: 'agent_status', agent: 'a1', node: 'n1', status: 'Starting' });
    render(<Agents />);
    fireEvent.click(screen.getByText('a1').closest('button')!);
    expect(screen.queryByTestId('term')).toBeNull();
    expect(screen.getByRole('status').textContent).toContain('Starting…');
    expect(sent.filter((f) => f.type === 'attach')).toEqual([]);
    act(() => feed({ type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' }));
    expect(screen.getByTestId('term')).toBeTruthy();
    expect(sent.filter((f) => f.type === 'attach')).toEqual([{ type: 'attach', agent: 'a1' }]);
  });

  it('marks agents with an open question as Asking', () => {
    feed({ type: 'question_opened', id: 'q', from: 'a1', text: 'x?', at_ms: 1 });
    render(<Agents />);
    expect(screen.getByText('a1').closest('button')!.textContent).toContain('Asking');
  });

  it('shows empty states', () => {
    freshStore();
    render(<Agents />);
    expect(screen.getByText('No nodes connected')).toBeTruthy();
    expect(screen.getByText('No agents yet')).toBeTruthy();
  });

  it('select attaches, switching detaches the previous first', () => {
    const { rerender } = render(<Agents />);
    fireEvent.click(screen.getByText('a1'));
    expect(sent).toEqual([{ type: 'attach', agent: 'a1' }]);
    rerender(<Agents />);
    expect(screen.getByRole('region', { name: 'Terminal of a1' })).toBeTruthy();
    expect(screen.getByTestId('term').textContent).toBe('a1');
    fireEvent.click(screen.getByText('a2'));
    expect(sent.slice(1)).toEqual([{ type: 'detach', agent: 'a1' }, { type: 'attach', agent: 'a2' }]);
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
    expect(sent[0]).toMatchObject({ type: 'pane_input', agent: 'a1' });
    expect(bytes(sent[0])).toEqual([0x68, 0x69, 0x0d]);
    expect(input.value).toBe('');
  });

  it('empty input sends nothing', () => {
    open();
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(sent).toEqual([]);
  });

  it('key bar buttons send their escape sequences', () => {
    open();
    const bar = within(screen.getByLabelText('Keys'));
    for (const [label, seq] of KEYS) {
      sent.length = 0;
      fireEvent.click(bar.getByRole('button', { name: label }));
      expect(bytes(sent[0]), label).toEqual(Array.from(new TextEncoder().encode(seq)));
    }
    expect(KEYS.find(([l]) => l === 'Ctrl+C')![1]).toBe('\x03');
    expect(KEYS.find(([l]) => l === 'Up')![1]).toBe('\x1b[A');
  });

  it('Detach sends detach and clears the selection', () => {
    open();
    fireEvent.click(screen.getByRole('button', { name: 'Detach' }));
    expect(sent).toEqual([{ type: 'detach', agent: 'a1' }]);
    expect(store.state.value.selected).toBeNull();
  });

  it('Kill needs two clicks', () => {
    open();
    fireEvent.click(screen.getByRole('button', { name: 'Kill' }));
    expect(sent).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Click again to kill' }));
    expect(sent).toEqual([{ type: 'kill', agent: 'a1' }]);
    expect(ofType(sent, 'kill')).toHaveLength(1);
  });
});
