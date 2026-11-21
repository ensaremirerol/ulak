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
    expect(card.textContent).toContain('claude@/cfg on box is not logged in');
    expect(card.textContent).toContain('run claude /login');
  });

  it('Open agent selects the agent and navigates to #/agents', () => {
    feed({ type: 'blocker_opened', ...blocker('b1', 'n1') });
    render(<Inbox />);
    fireEvent.click(screen.getByRole('button', { name: 'Open terminal to log in' }));
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

  it('install failed: retry the right profile, stop the paused agents', () => {
    feed(
      { type: 'node_profiles', node: 'n1', profiles: [{ name: 'claude-work', binary: 'claude', config_dir: '/cfg', installed: false, login_blocked: false, installable: true }] },
      { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Installing' },
      { type: 'blocker_opened', ...blocker('b1', 'n1', { kind: 'Install', profile: 'claude@/cfg', detail: 'npm error EACCES' }) },
    );
    render(<Inbox />);
    const card = screen.getByRole('article', { name: 'Install failed on box' });
    expect(card.textContent).toContain('claude could not be installed on box');
    expect(card.textContent).toContain('paused: a1');
    fireEvent.click(screen.getByRole('button', { name: 'Retry install' }));
    expect(sent).toEqual([{ type: 'install_profile', node: 'n1', name: 'claude-work' }]);
    fireEvent.click(screen.getByRole('button', { name: 'Stop the paused agents' }));
    fireEvent.click(screen.getByRole('button', { name: 'Click again to stop 1' }));
    expect(sent[1]).toEqual({ type: 'kill', agent: 'a1' });
  });

  it('a forward nobody owns: send it to a running agent or leave it in Issues', () => {
    feed(
      { type: 'agent_status', agent: 'a1', node: 'n1', status: 'Running' },
      { type: 'forwards', forwards: [{ id: 'f1', issue: 'o/n#212', from: 'tests-fix', to: null, reason: 'not mine', status: 'needs_owner', note: 'No routing rule matched, so it came to you.', title: 'Drops tags', at_ms: 1 }] },
    );
    render(<Inbox />);
    const card = screen.getByRole('article', { name: 'Forward of o/n#212' });
    expect(card.textContent).toContain('tests-fix forwarded o/n#212');
    fireEvent.click(screen.getByRole('button', { name: 'Send to a1' }));
    expect(sent).toEqual([{ type: 'resolve_forward', id: 'f1', to: 'a1' }]);
    fireEvent.click(screen.getByRole('button', { name: 'Leave it in Issues' }));
    expect(sent[1]).toEqual({ type: 'resolve_forward', id: 'f1', to: null });
    fireEvent.click(screen.getByRole('button', { name: 'Start an agent for #212' }));
    expect(screen.getByRole('dialog', { name: 'New agent for an issue' })).toBeTruthy();
  });

  it('nav badge counts questions plus blockers', () => {
    feed({ type: 'question_opened', id: 'q1', from: 'a1', text: '?', at_ms: 1 }, { type: 'blocker_opened', ...blocker('b1', 'n1') });
    render(<Shell><Inbox /></Shell>);
    expect(screen.getByLabelText('2 waiting').textContent).toBe('2');
  });
});
