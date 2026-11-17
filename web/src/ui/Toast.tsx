import type { Toast } from '../state';

export interface ToastHostProps {
  toasts: readonly Toast[];
  onDismiss(id: number): void;
}

/** Polite live region; error toasts are announced assertively (role=alert). */
export function ToastHost({ toasts, onDismiss }: ToastHostProps) {
  return (
    <div class="toasts" role="region" aria-label="Notifications">
      {toasts.map((t) => (
        <div key={t.id} class={`toast ${t.kind}`} role={t.kind === 'error' ? 'alert' : 'status'}>
          <span class="toast-text">{t.text}</span>
          <button type="button" class="btn sm ghost" aria-label="Dismiss" onClick={() => onDismiss(t.id)}>x</button>
        </div>
      ))}
    </div>
  );
}
