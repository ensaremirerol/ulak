import { useEffect, useState } from 'preact/hooks';
import { Button } from './components';

export const CONFIRM_MS = 4000;

/** Two-click action: the first click arms (label changes), the second sends. Disarms after a few seconds and on blur. */
export function ConfirmButton({ label, confirmLabel = 'Click again to confirm', variant = 'danger', large, disabled, onConfirm }: {
  label: string;
  confirmLabel?: string;
  variant?: 'default' | 'primary' | 'ghost' | 'danger';
  large?: boolean;
  disabled?: boolean;
  onConfirm(): void;
}) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), CONFIRM_MS);
    return () => clearTimeout(t);
  }, [armed]);
  return (
    <Button
      variant={variant}
      size={large ? undefined : 'sm'}
      disabled={disabled}
      onBlur={() => setArmed(false)}
      onClick={() => {
        if (!armed) return setArmed(true);
        setArmed(false);
        onConfirm();
      }}
    >
      {armed ? confirmLabel : label}
    </Button>
  );
}
