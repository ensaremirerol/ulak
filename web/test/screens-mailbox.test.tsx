import { act, fireEvent, render, screen, within } from '@testing-library/preact';
import { beforeEach, describe, expect, it } from 'vitest';
import type { Message, UiToHub } from '../src/proto';
import { Mailbox } from '../src/screens/Mailbox';
import { MAILBOX_LIMIT } from '../src/state';
import { feed, freshStore } from './helpers';

const msg = (id: string, thread: string, at_ms: number, over: Partial<Message> = {}): Message =>
  ({ id, from: 'a1', to: 'a2', thread, payload: `body-${id}`, status: 'Pending', at_ms, ...over });
let sent: UiToHub[];
beforeEach(() => { sent = freshStore(); });

describe('Mailbox', () => {
  it('empty state, and refresh on open', () => {
    render(<Mailbox />);
    expect(screen.getByText('No messages')).toBeTruthy();
    expect(sent).toEqual([{ type: 'mailbox_log', thread: null, limit: MAILBOX_LIMIT }]);
  });

  it('lists newest first with status and truncated previews', () => {
    feed({ type: 'mailbox_log', messages: [msg('m1', 'aaaaaaaa-1111', 1000, { status: 'Read' }), msg('m2', 'bbbbbbbb-2222', 2000, { payload: 'x'.repeat(200) })] });
    render(<Mailbox />);
    const rows = within(screen.getByRole('region', { name: 'Messages' })).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain('a1 -> a2');
    expect(rows[0]!.textContent).toContain('x'.repeat(120) + '...');
    expect(rows[0]!.textContent).not.toContain('x'.repeat(121));
    expect(rows[1]!.textContent).toContain('Read');
    expect(rows[1]!.textContent).toContain('aaaaaaaa');
  });

  it('thread filter, request with thread, and Clear filter', () => {
    feed({ type: 'mailbox_log', messages: [msg('m1', 'aaaaaaaa-1111', 1000), msg('m2', 'bbbbbbbb-2222', 2000)] });
    render(<Mailbox />);
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Filter thread aaaaaaaa-1111' }));
    expect(sent).toEqual([{ type: 'mailbox_log', thread: 'aaaaaaaa-1111', limit: MAILBOX_LIMIT }]);
    expect(screen.queryByText('body-m2')).toBeNull();
    expect(screen.getByText('body-m1')).toBeTruthy();
    sent.length = 0;
    fireEvent.click(screen.getByRole('button', { name: 'Refresh' }));
    expect(sent).toEqual([{ type: 'mailbox_log', thread: 'aaaaaaaa-1111', limit: MAILBOX_LIMIT }]);
    fireEvent.click(screen.getByRole('button', { name: 'Clear filter' }));
    expect(sent.at(-1)).toEqual({ type: 'mailbox_log', thread: null, limit: MAILBOX_LIMIT });
    expect(screen.getByText('body-m2')).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Clear filter' })).toBeNull();
  });

  it('filter with no matches says so', () => {
    feed({ type: 'mailbox_log', messages: [msg('m1', 'aaaaaaaa-1111', 1000)] });
    render(<Mailbox />);
    fireEvent.click(screen.getByRole('button', { name: 'Filter thread aaaaaaaa-1111' }));
    act(() => feed({ type: 'mailbox_log', messages: [] }));
    expect(screen.getByText('Nothing in this thread.')).toBeTruthy();
  });
});
