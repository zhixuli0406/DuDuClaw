import { useCallback, useRef, useState, type ReactNode } from 'react';
import { stepUpApi, type StepUpToken } from '@/lib/step-up';
import { StepUpDialog } from './StepUpDialog';

/**
 * Admin actions on the restore screens: `ensure()` resolves the step-up
 * token, asking for password + code only when the five-minute proof is not
 * valid any more. Render `dialog` once in the screen.
 */
export function useStepUpToken(): { ensure: () => Promise<StepUpToken | null>; dialog: ReactNode } {
  const [open, setOpen] = useState(false);
  const resolver = useRef<((token: StepUpToken | null) => void) | null>(null);

  const ensure = useCallback(async () => {
    try {
      const status = await stepUpApi.status();
      if (status.step_up_valid && status.csrf_token) return status.csrf_token;
    } catch {
      // Fall through: the dialog reports what it can.
    }
    return new Promise<StepUpToken | null>((resolve) => {
      resolver.current?.(null);
      resolver.current = resolve;
      setOpen(true);
    });
  }, []);

  const done = useCallback((token: StepUpToken | null) => {
    setOpen(false);
    const resolve = resolver.current;
    resolver.current = null;
    resolve?.(token);
  }, []);

  return { ensure, dialog: <StepUpDialog open={open} purpose="admin" onDone={done} /> };
}
