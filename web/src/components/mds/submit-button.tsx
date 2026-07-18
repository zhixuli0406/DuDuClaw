import { forwardRef } from 'react';
import { ArrowUpIcon, Loader2Icon, SquareIcon } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button, type ButtonProps } from './button';

/**
 * SubmitButton — MDS composer send control (spec §4/§5.6). Three states:
 * `idle` → send (ArrowUp), `submitting` → busy (spinning Loader2, aria-busy),
 * `streaming` → stop (Square). In `submitting` the button is disabled; in
 * `streaming` it stays clickable so the caller can cancel the stream.
 */
export type SubmitButtonState = 'idle' | 'submitting' | 'streaming';

const LABELS: Record<SubmitButtonState, string> = {
  idle: 'Send',
  submitting: 'Sending',
  streaming: 'Stop',
};

export const SubmitButton = forwardRef<
  HTMLButtonElement,
  Omit<ButtonProps, 'children'> & { state?: SubmitButtonState }
>(({ state = 'idle', className, disabled, variant = 'brand', size = 'icon', ...props }, ref) => {
  const busy = state === 'submitting';
  return (
    <Button
      ref={ref}
      variant={variant}
      size={size}
      aria-label={LABELS[state]}
      aria-busy={busy || undefined}
      data-state={state}
      disabled={disabled || busy}
      className={cn('rounded-full', className)}
      {...props}
    >
      {state === 'idle' && <ArrowUpIcon />}
      {state === 'submitting' && <Loader2Icon className="animate-spin" />}
      {state === 'streaming' && <SquareIcon className="fill-current" />}
    </Button>
  );
});
SubmitButton.displayName = 'SubmitButton';
