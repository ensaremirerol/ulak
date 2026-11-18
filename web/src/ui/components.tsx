import type { ButtonHTMLAttributes, ComponentChildren } from 'preact';
import { useId } from 'preact/hooks';

export type Tone = 'ok' | 'info' | 'warn' | 'bad' | 'muted';

export function Button({ variant = 'default', size, class: c, ...rest }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: 'default' | 'primary' | 'ghost' | 'danger'; size?: 'sm' }) {
  return <button type="button" {...rest} class={['btn', variant !== 'default' && variant, size, c].filter(Boolean).join(' ')} />;
}

export const Pill = ({ tone = 'muted', title, children }: { tone?: Tone; title?: string; children: ComponentChildren }) => <span class={`pill ${tone}`} title={title}>{children}</span>;

/** Count badge (nav); renders nothing for 0. */
export const Badge = ({ count }: { count: number }) => (count > 0 ? <span class="badge" aria-label={`${count} waiting`}>{count}</span> : null);

export const Dot = ({ tone = 'muted' }: { tone?: Tone }) => <span class={`dot ${tone}`} aria-hidden="true" />;

export function Card({ title, aside, dashed, children }: { title?: string; aside?: ComponentChildren; dashed?: boolean; children?: ComponentChildren }) {
  return (
    <section class={`card${dashed ? ' dashed' : ''}`}>
      {title && <div class="card-title"><h2>{title}</h2>{aside}</div>}
      {children}
    </section>
  );
}

/** 0-100 meter; null shows "n/a". Turns warn >= 70, bad >= 90. */
export function ProgressBar({ label, value }: { label: string; value: number | null }) {
  const pct = value === null ? 0 : Math.max(0, Math.min(100, Math.round(value)));
  const tone = pct >= 90 ? ' bad' : pct >= 70 ? ' warn' : '';
  return (
    <div class="bar">
      <span class="bar-label">{label}</span>
      <span class="bar-track" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={value === null ? undefined : pct}>
        <span class={`bar-fill${tone}`} style={{ width: `${pct}%` }} />
      </span>
      <span>{value === null ? 'n/a' : `${pct}%`}</span>
    </div>
  );
}

export interface TextFieldProps {
  label: string;
  value: string;
  placeholder?: string;
  autocomplete?: string;
  autocapitalize?: 'off' | 'characters';
  spellcheck?: boolean;
  disabled?: boolean;
  /** Marks the label (" *" via CSS) and sets aria-required. */
  required?: boolean;
  hint?: string;
  error?: string | null;
  mono?: boolean;
  onValue(v: string): void;
}

/** Labelled input; hint and error are linked via aria-describedby. */
export function TextField({ label, hint, error, mono, onValue, ...rest }: TextFieldProps) {
  const id = useId();
  const note = error ? `${id}-e` : hint ? `${id}-h` : undefined;
  return (
    <div class="field">
      <label for={id} class={rest.required ? 'req' : undefined}>{label}</label>
      <input {...rest} aria-required={rest.required ? true : undefined} type="text" id={id} class={`input${mono ? ' mono' : ''}`} aria-invalid={error ? true : undefined} aria-describedby={note} onInput={(e) => onValue(e.currentTarget.value)} />
      {error ? <span id={note} class="err" role="alert">{error}</span> : hint ? <span id={note} class="hint">{hint}</span> : null}
    </div>
  );
}

export const EmptyState = ({ title, children, action }: { title: string; children?: ComponentChildren; action?: ComponentChildren }) => (
  <div class="empty"><h2>{title}</h2>{children && <p>{children}</p>}{action}</div>
);
