import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import type { StepUpToken } from '@/lib/step-up';
import { AdminStepUpDialog } from './AdminStepUpDialog';

const CACHE_MS = 4.5 * 60 * 1000;

/** Render dialog once. The proof belongs only to this mounted hook instance. */
export function useAdminStepUp(): {
  ensure: (reason?: string) => Promise<StepUpToken | null>;
  invalidate: () => void;
  dialog: ReactNode;
} {
  const cache = useRef<{ token: StepUpToken; obtainedAt: number } | null>(null);
  const pending = useRef<{ promise: Promise<StepUpToken | null>; resolve: (token: StepUpToken | null) => void } | null>(null);
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState<string>();

  const invalidate = useCallback(() => { cache.current = null; }, []);
  useEffect(() => () => {
    cache.current = null;
    pending.current?.resolve(null);
    pending.current = null;
  }, []);

  const ensure = useCallback((nextReason?: string): Promise<StepUpToken | null> => {
    const cached = cache.current;
    if (cached && Date.now() - cached.obtainedAt >= 0 && Date.now() - cached.obtainedAt < CACHE_MS) {
      return Promise.resolve(cached.token);
    }
    cache.current = null;
    if (pending.current) return pending.current.promise;
    let resolve!: (token: StepUpToken | null) => void;
    const promise = new Promise<StepUpToken | null>((done) => { resolve = done; });
    pending.current = { promise, resolve };
    setReason(nextReason);
    setOpen(true);
    return promise;
  }, []);

  const done = useCallback((token: StepUpToken | null) => {
    if (!pending.current) return;
    cache.current = token ? { token, obtainedAt: Date.now() } : null;
    const { resolve } = pending.current;
    pending.current = null;
    setOpen(false);
    resolve(token);
  }, []);

  return { ensure, invalidate, dialog: <AdminStepUpDialog open={open} reason={reason} onDone={done} /> };
}
