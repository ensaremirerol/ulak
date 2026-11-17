import { useRef } from 'preact/hooks';

export interface SegmentedProps<T extends string> {
  label: string;
  value: T;
  options: readonly { value: T; label: string; disabled?: boolean }[];
  onChange(v: T): void;
}

/** Radio group with roving tabindex; arrow keys move and select (wrapping), Home/End jump. */
export function Segmented<T extends string>({ label, value, options, onChange }: SegmentedProps<T>) {
  const box = useRef<HTMLDivElement>(null);
  const onKey = (e: KeyboardEvent) => {
    const i = options.findIndex((o) => o.value === value);
    const step = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
    const to = e.key === 'Home' ? 0 : e.key === 'End' ? options.length - 1 : step ? (i + step + options.length) % options.length : -1;
    if (to < 0 || options[to]!.disabled) return;
    e.preventDefault();
    onChange(options[to]!.value);
    box.current?.querySelectorAll<HTMLElement>('button')[to]?.focus();
  };
  return (
    <div class="seg" role="radiogroup" aria-label={label} ref={box} onKeyDown={onKey}>
      {options.map((o) => (
        <button key={o.value} type="button" role="radio" aria-checked={o.value === value} tabIndex={o.value === value ? 0 : -1} disabled={o.disabled} onClick={() => onChange(o.value)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}
