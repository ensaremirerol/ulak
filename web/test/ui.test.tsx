import { fireEvent, render, screen, waitFor } from '@testing-library/preact';
import { useState } from 'preact/hooks';
import { describe, expect, it, vi } from 'vitest';
import { Dialog } from '../src/ui/Dialog';
import { Segmented } from '../src/ui/Segmented';
import { ToastHost } from '../src/ui/Toast';
import { Pill, ProgressBar } from '../src/ui/components';
import { cursorScrollTop } from '../src/ui/scroll';
import { parseRoute } from '../src/ui/router';

describe('Dialog', () => {
  const setup = (onClose = vi.fn()) => {
    render(<><button>opener</button><Dialog title="Confirm" onClose={onClose}><input aria-label="one" /><button>last</button></Dialog></>);
    return onClose;
  };
  it('focuses the first control and traps Tab both ways', () => {
    setup();
    const one = screen.getByLabelText('one');
    const last = screen.getByText('last');
    expect(document.activeElement).toBe(one);
    last.focus();
    fireEvent.keyDown(last, { key: 'Tab' });
    expect(document.activeElement).toBe(one);
    fireEvent.keyDown(one, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(last);
  });
  it('closes on Escape and exposes dialog semantics', () => {
    const onClose = setup();
    expect(screen.getByRole('dialog', { name: 'Confirm' }).getAttribute('aria-modal')).toBe('true');
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledOnce();
  });
  it('returns focus to the opener on unmount', async () => {
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    const { unmount } = render(<Dialog title="t" onClose={() => {}}><button>x</button></Dialog>);
    unmount();
    await waitFor(() => expect(document.activeElement).toBe(opener));
    opener.remove();
  });
});

describe('Segmented', () => {
  const opts = [{ value: 'a', label: 'A' }, { value: 'b', label: 'B' }, { value: 'c', label: 'C' }] as const;
  const Wrap = () => {
    const [v, setV] = useState<'a' | 'b' | 'c'>('a');
    return <Segmented<'a' | 'b' | 'c'> label="Pick" value={v} options={opts} onChange={setV} />;
  };
  it('arrow keys move and wrap, Home/End jump, roving tabindex', () => {
    render(<Wrap />);
    const radio = (n: string) => screen.getByRole('radio', { name: n });
    expect(radio('A').getAttribute('aria-checked')).toBe('true');
    expect(radio('B').tabIndex).toBe(-1);
    fireEvent.keyDown(radio('A'), { key: 'ArrowRight' });
    expect(radio('B').getAttribute('aria-checked')).toBe('true');
    expect(document.activeElement).toBe(radio('B'));
    fireEvent.keyDown(radio('B'), { key: 'End' });
    expect(radio('C').getAttribute('aria-checked')).toBe('true');
    fireEvent.keyDown(radio('C'), { key: 'ArrowRight' });
    expect(radio('A').getAttribute('aria-checked')).toBe('true');
    fireEvent.keyDown(radio('A'), { key: 'ArrowLeft' });
    expect(radio('C').getAttribute('aria-checked')).toBe('true');
  });
  it('click selects', () => {
    render(<Wrap />);
    fireEvent.click(screen.getByRole('radio', { name: 'C' }));
    expect(screen.getByRole('radio', { name: 'C' }).getAttribute('aria-checked')).toBe('true');
  });
});

describe('ToastHost', () => {
  it('renders toasts with the right roles and dismisses by id', () => {
    const onDismiss = vi.fn();
    render(<ToastHost onDismiss={onDismiss} toasts={[{ id: 7, kind: 'error', text: 'boom', expires: null }, { id: 8, kind: 'ok', text: 'fine', expires: null }]} />);
    expect(screen.getByRole('alert').textContent).toContain('boom');
    expect(screen.getByRole('status').textContent).toContain('fine');
    fireEvent.click(screen.getAllByRole('button', { name: 'Dismiss' })[0]!);
    expect(onDismiss).toHaveBeenCalledWith(7);
  });
});

describe('misc', () => {
  it('ProgressBar clamps and shows n/a', () => {
    render(<><ProgressBar label="CPU" value={140} /><ProgressBar label="GPU" value={null} /></>);
    expect(screen.getByRole('progressbar', { name: 'CPU' }).getAttribute('aria-valuenow')).toBe('100');
    expect(screen.getByText('n/a')).toBeTruthy();
  });
  it('parseRoute falls back to agents', () => {
    expect(parseRoute('#/inbox')).toBe('inbox');
    expect(parseRoute('#/devices/x')).toBe('devices');
    expect(parseRoute('#/nope')).toBe('agents');
    expect(parseRoute('')).toBe('agents');
  });
});

describe('cursorScrollTop', () => {
  it('keeps the scroll while the cursor row is visible, else brings it to the bottom edge', () => {
    expect(cursorScrollTop(5, 20, 0, 400)).toBe(0); // short content: stay at the top
    expect(cursorScrollTop(10, 20, 100, 400)).toBe(100);
    expect(cursorScrollTop(49, 20, 0, 400)).toBe(600);
    expect(cursorScrollTop(0, 20, 600, 400)).toBe(0);
    expect(cursorScrollTop(3, 20, 0, 1000)).toBe(0);
  });
});

describe('node state pill', () => {
  it('never wraps and carries the full text as title', () => {
    render(<Pill tone="info" title="Gaming: ulak-busy">Gaming: ulak-busy</Pill>);
    expect(screen.getByText('Gaming: ulak-busy').getAttribute('title')).toBe('Gaming: ulak-busy');
  });
});
