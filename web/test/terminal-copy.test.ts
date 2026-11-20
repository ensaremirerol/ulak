import type { Terminal as XTerm } from '@xterm/xterm';
import { describe, expect, it } from 'vitest';
import { isCopyKey, termText } from '../src/ui/Terminal';

const key = (k: string, mods: Partial<KeyboardEvent> = {}) => ({ type: 'keydown', key: k, ctrlKey: false, metaKey: false, ...mods }) as KeyboardEvent;
const fake = (sel: string, lines: string[]) => ({
  hasSelection: () => sel !== '',
  getSelection: () => sel,
  buffer: { active: { length: lines.length, getLine: (y: number) => ({ translateToString: () => lines[y] }) } },
}) as unknown as XTerm;

describe('terminal copy', () => {
  it('Ctrl/Cmd+C copies only with a selection; Ctrl+C alone stays the interrupt', () => {
    expect(isCopyKey(key('c', { ctrlKey: true }), true)).toBe(true);
    expect(isCopyKey(key('C', { metaKey: true }), true)).toBe(true);
    expect(isCopyKey(key('c', { ctrlKey: true }), false)).toBe(false);
    expect(isCopyKey(key('c'), true)).toBe(false);
    expect(isCopyKey({ ...key('c', { ctrlKey: true }), type: 'keyup' } as KeyboardEvent, true)).toBe(false);
  });

  it('copies the selection, else the screen without trailing blank lines', () => {
    expect(termText(fake('sel', ['a', 'b']))).toBe('sel');
    expect(termText(fake('', ['a', 'b', '', '']))).toBe('a\nb');
  });
});
