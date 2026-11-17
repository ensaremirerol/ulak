import type { ComponentChildren } from 'preact';
import { useEffect, useRef } from 'preact/hooks';

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export interface DialogProps {
  title: string;
  onClose(): void;
  children: ComponentChildren;
  /** Buttons row. */
  actions?: ComponentChildren;
}

/** Modal dialog: focus moves in, Tab cycles inside, Escape closes, focus returns to the opener. Mount it only while open. */
export function Dialog({ title, onClose, children, actions }: DialogProps) {
  const box = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  close.current = onClose;

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const items = () => Array.from(box.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? []);
    (items()[0] ?? box.current)?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        close.current();
      } else if (e.key === 'Tab') {
        const list = items();
        if (list.length === 0) return e.preventDefault();
        const first = list[0]!;
        const last = list[list.length - 1]!;
        const at = document.activeElement;
        if (e.shiftKey && (at === first || !box.current?.contains(at))) { e.preventDefault(); last.focus(); }
        else if (!e.shiftKey && (at === last || !box.current?.contains(at))) { e.preventDefault(); first.focus(); }
      }
    };
    document.addEventListener('keydown', onKey, true);
    return () => {
      document.removeEventListener('keydown', onKey, true);
      opener?.focus?.();
    };
  }, []);

  return (
    <div class="backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div class="dialog" role="dialog" aria-modal="true" aria-label={title} ref={box} tabIndex={-1}>
        <h2>{title}</h2>
        {children}
        {actions && <div class="dialog-actions">{actions}</div>}
      </div>
    </div>
  );
}
