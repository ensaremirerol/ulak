import { FitAddon } from '@xterm/addon-fit';
import { Terminal as XTerm } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import type { Ref } from 'preact';
import { useEffect, useImperativeHandle, useRef } from 'preact/hooks';
import { utf8 } from '../proto';
import { store } from '../state';
import { termBytes } from '../state/reducer';
import { cursorScrollTop } from './scroll';

// Nodes before 0.1.10 ignore `resize` and keep every tmux pane at 200x50 (tmux.rs): for them render at exactly that
// size and scroll the host, as fitting would wrap/crop the redraw. Newer nodes resize the pane to the fitted terminal.
const COLS = 200, ROWS = 50;
/** Wait after the last size change before telling the node (a drag resizes many times). */
const RESIZE_DEBOUNCE_MS = 150;

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
export function Terminal({ agent, fit, handle }: { agent: string; fit: boolean; handle?: Ref<TerminalHandle> }) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<XTerm | null>(null);
  const fitter = useRef<FitAddon | null>(null);
  useImperativeHandle(handle ?? null, () => ({ text: () => (term.current ? termText(term.current) : '') }), []);

  useEffect(() => {
    adoptInjectedStyles();
    const t = new XTerm({
      fontFamily: css('--font-mono') || 'monospace',
      ...(fit ? {} : { cols: COLS, rows: ROWS }),
      fontSize: 13,
      cursorBlink: true,
      scrollback: 5000,
      theme: {
        background: css('--terminal'), foreground: css('--text'), cursor: css('--accent'), selectionBackground: css('--border-strong'),
        red: css('--bad'), green: css('--ok'), yellow: css('--warn'), blue: css('--info'),
      },
    });
    if (fit) {
      fitter.current = new FitAddon();
      t.loadAddon(fitter.current);
    }
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
      fitter.current = null;
    };
  }, [fit]);

  // Fit to the box and tell the node; again whenever the box changes size (window, fullscreen, pop-out).
  useEffect(() => {
    const f = fitter.current, h = host.current;
    if (!fit || !f || !h) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let sent = '';
    const refit = () => {
      const d = f.proposeDimensions();
      if (!d || !Number.isFinite(d.cols) || !Number.isFinite(d.rows) || d.cols < 2 || d.rows < 2) return; // hidden box
      f.fit();
      clearTimeout(timer);
      timer = setTimeout(() => {
        const key = `${d.cols}x${d.rows}`;
        if (key === sent) return;
        sent = key;
        store.intent({ kind: 'resize', agent, cols: d.cols, rows: d.rows });
      }, RESIZE_DEBOUNCE_MS);
    };
    refit();
    const ro = new ResizeObserver(refit);
    ro.observe(h);
    return () => {
      ro.disconnect();
      clearTimeout(timer);
    };
  }, [agent, fit]);

  useEffect(() => {
    const t = term.current!;
    t.reset();
    const old = termBytes(store.state.peek().terminals[agent]);
    const follow = () => {
      const h = host.current;
      if (!h || fit) return; // a fitted terminal shows the whole pane
      h.scrollTop = cursorScrollTop(t.buffer.active.cursorY, h.scrollHeight / ROWS, h.scrollTop, h.clientHeight);
    };
    if (old.length > 0) t.write(old, follow);
    t.focus();
    // Every chunk is a full redraw, which wipes xterm's selection: while the user drags or has text selected, hold
    // the redraws and apply only the latest once the selection is gone.
    let held: Uint8Array | null = null;
    let dragging = false;
    const holding = () => dragging || t.hasSelection();
    const flush = () => {
      if (held && !holding()) {
        t.write(held, follow);
        held = null;
      }
    };
    const h = host.current!;
    const down = () => { dragging = true; };
    const up = () => {
      dragging = false;
      flush();
    };
    h.addEventListener('mousedown', down);
    window.addEventListener('mouseup', up);
    const sel = t.onSelectionChange(flush);
    const off = store.onPane(agent, (d) => {
      if (holding()) held = d;
      else t.write(d, follow);
    });
    return () => {
      off();
      sel.dispose();
      h.removeEventListener('mousedown', down);
      window.removeEventListener('mouseup', up);
    };
  }, [agent, fit]);

  return <div class={`term-host${fit ? ' term-fit' : ''}`} ref={host} />;
}
