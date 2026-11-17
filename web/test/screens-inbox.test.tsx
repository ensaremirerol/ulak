import { fireEvent, render, screen } from '@testing-library/preact';
import { beforeEach, describe, expect, it } from 'vitest';
import type { UiToHub } from '../src/proto';
import { Inbox } from '../src/screens/Inbox';
import { store } from '../src/state';
import { Shell } from '../src/ui/Shell';
import { blocker, feed, freshStore, node } from './helpers';

let sent: UiToHub[];
beforeEach(() => {
  sent = freshStore();
  location.hash = '';
  feed({ type: 'node_update', ...node('n1', 'box', ['a1']) });
});

describe('Inbox', () => {
  it('empty state', () => {
    render(<Inbox />);
    expect(screen.getByText('Nothing waiting')).toBeTruthy();
  });

  it('question: answer via Enter (form submit) and via Send click, then it closes', () => {
    feed({ type: 'question_opened', id: 'q1', from: 'a1', text: 'Which db?', at_ms: 1 });
    const { rerender } = render(<Inbox />);
    const card = screen.getByRole('article', { name: 'Question from a1' });
    expect(card.textContent).toContain('Which db?');
    const input = screen.getByLabelText('Answer to a1');
    fireEvent.input(input, { target: { value: 'postgres' } });
    fireEvent.submit(input.closest('form')!);
    expect(sent).toEqual([{ type: 'answer', question: 'q1', text: 'postgres' }]);
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(sent[1]).toEqual({ type: 'answer', question: 'q1', text: 'postgres' });
    feed({ type: 'question_closed', id: 'q1' });
    rerender(<Inbox />);
    expect(screen.queryByRole('article', { name: 'Question from a1' })).toBeNull();
  });

  it('empty answer is refused with an error toast', () => {
    feed({ type: 'question_opened', id: 'q1', from: 'a1', text: '?', at_ms: 1 });
    render(<Inbox />);
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    expect(sent).toEqual([]);
    expect(store.state.value.toasts[0]?.text).toContain('empty');
  });

  it('blocker card shows kind, node, profile and detail', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1', { profile: 'claude@/cfg', detail: 'run claude /login' }) });
    render(<Inbox />);
    const card = screen.getByRole('article', { name: 'Login required on box' });
    expect(card.textContent).toContain('claude on box');
    expect(card.textContent).toContain('run claude /login');
  });

  it('Open agent selects the agent and navigates to #/agents', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1') });
    render(<Inbox />);
    fireEvent.click(screen.getByRole('button', { name: 'Open agent' }));
    expect(sent).toEqual([{ type: 'attach', agent: 'a1' }]);
    expect(location.hash).toBe('#/agents');
  });

  it('Mark resolved needs two clicks', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1', { kind: 'Install' }) });
    render(<Inbox />);
    expect(screen.getByRole('article', { name: 'Install failed on box' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Mark resolved' }));
    expect(sent).toEqual([]);
    fireEvent.click(screen.getByRole('button', { name: 'Click again to resolve' }));
    expect(sent).toEqual([{ type: 'resolve_blocker', id: 'b1' }]);
  });

  it('nav badge counts questions plus blockers', () => {
    feed({ type: 'question_opened', id: 'q1', from: 'a1', text: '?', at_ms: 1 }, { type: 'blocker_opened', ...blocker('b1', 'n1') });
    render(<Shell><Inbox /></Shell>);
    expect(screen.getByLabelText('2 waiting').textContent).toBe('2');
  });
});
