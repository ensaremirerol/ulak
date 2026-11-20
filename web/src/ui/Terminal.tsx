import { Terminal as XTerm } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import type { Ref } from 'preact';
import { useEffect, useImperativeHandle, useRef } from 'preact/hooks';
import { utf8 } from '../proto';
import { store } from '../state';
import { termBytes } from '../state/reducer';
import { cursorScrollTop } from './scroll';

// The protocol has no resize frame and ulakd creates every tmux pane at 200x50 (tmux.rs), so render at exactly that size
// and scroll the host instead of fitting: fitting to the window would wrap/crop the redraw.
const COLS = 200, ROWS = 50;

/**
 * xterm.js injects <style> elements at runtime; the hub CSP (style-src 'self') blocks them. A blocked element still keeps
 * its text, so mirror it into a constructed stylesheet (adoptedStyleSheets is not governed by style-src) and keep it in
 * sync, as xterm rewrites the text on theme and size changes. The CSP stays unchanged.
 */
const mirrored = new Map<HTMLStyleElement, CSSStyleSheet>();
function mirror(el: HTMLStyleElement) {
  let sheet = mirrored.get(el);
  if (!sheet) {
    sheet = new CSSStyleSheet();
    mirrored.set(el, sheet);
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  }
  sheet.replaceSync(el.textContent ?? '');
}
function unmirror(el: HTMLStyleElement) {
  const sheet = mirrored.get(el);
  if (!sheet) return;
  mirrored.delete(el);
  document.adoptedStyleSheets = document.adoptedStyleSheets.filter((x) => x !== sheet);
}
let observing = false;
function adoptInjectedStyles() {
  if (observing) return;
  observing = true;
  new MutationObserver((muts) => {
    for (const m of muts) {
      const styleOf = (n: Node) => (n instanceof HTMLStyleElement ? n : n.parentElement instanceof HTMLStyleElement ? n.parentElement : null);
      m.addedNodes.forEach((n) => {
        const el = styleOf(n);
        if (el) mirror(el);
        else if (n instanceof Element) n.querySelectorAll('style').forEach(mirror);
      });
      m.removedNodes.forEach((n) => {
        if (n instanceof HTMLStyleElement) unmirror(n);
        else if (n instanceof Element) n.querySelectorAll('style').forEach(unmirror);
      });
      const el = styleOf(m.target);
      if (el && el.isConnected) mirror(el);
    }
  }).observe(document.documentElement, { childList: true, subtree: true, characterData: true });
  document.querySelectorAll('style').forEach(mirror);
}

const css = (name: string) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/** The selection, else the whole screen (trailing blanks trimmed). */
export function termText(t: XTerm): string {
  if (t.hasSelection()) return t.getSelection();
  const b = t.buffer.active;
  const lines: string[] = [];
  for (let y = 0; y < b.length; y++) lines.push(b.getLine(y)?.translateToString(true) ?? '');
  return lines.join('\n').trimEnd();
}

/** Ctrl/Cmd+C with a selection copies it; without one Ctrl+C stays the interrupt key. */
export const isCopyKey = (e: KeyboardEvent, selected: boolean): boolean =>
  selected && e.type === 'keydown' && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'c';

export interface TerminalHandle { text(): string }

/** xterm.js bound to one agent: replays the buffered bytes, then writes chunks as they arrive. */
export function Terminal({ agent, handle }: { agent: string; handle?: Ref<TerminalHandle> }) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<XTerm | null>(null);
  useImperativeHandle(handle ?? null, () => ({ text: () => (term.current ? termText(term.current) : '') }), []);

  useEffect(() => {
    adoptInjectedStyles();
    const t = new XTerm({
      fontFamily: css('--font-mono') || 'monospace',
      cols: COLS,
      rows: ROWS,
      fontSize: 13,
      cursorBlink: true,
      scrollback: 5000,
      theme: {
        background: css('--terminal'), foreground: css('--text'), cursor: css('--accent'), selectionBackground: css('--border-strong'),
        red: css('--bad'), green: css('--ok'), yellow: css('--warn'), blue: css('--info'),
      },
    });
    t.open(host.current!);
    t.attachCustomKeyEventHandler((e) => {
      if (!isCopyKey(e, t.hasSelection())) return true;
      void navigator.clipboard?.writeText(t.getSelection());
      e.preventDefault();
      return false;
    });
    t.onData((d) => store.intent({ kind: 'send_keys', data: utf8(d) }));
    term.current = t;
    return () => {
      t.dispose();
      term.current = null;
    };
  }, []);

  useEffect(() => {
    const t = term.current!;
    t.reset();
    const old = termBytes(store.state.peek().terminals[agent]);
    const follow = () => {
      const h = host.current;
      if (!h) return;
      h.scrollTop = cursorScrollTop(t.buffer.active.cursorY, h.scrollHeight / ROWS, h.scrollTop, h.clientHeight);
    };
    if (old.length > 0) t.write(old, follow);
    t.focus();
    return store.onPane(agent, (d) => t.write(d, follow));
  }, [agent]);

  return <div class="term-host" ref={host} />;
}
