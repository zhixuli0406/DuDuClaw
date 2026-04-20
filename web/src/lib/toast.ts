/**
 * Lightweight toast event bus.
 *
 * The bus lets any module — including code paths that live outside the React
 * tree (API error handlers inside effects, utility modules, etc.) — dispatch
 * toast notifications without needing access to the `useToast()` hook.
 *
 * `ToastProvider` in `components/Toast.tsx` subscribes to this bus and
 * forwards `emit` payloads into its context-local `show()`. Immutable
 * listener array updates keep subscribe/unsubscribe safe under React 19
 * StrictMode double-invocation.
 */

export type ToastVariant = 'success' | 'error' | 'info';

export interface Toast {
  id: string;
  variant: ToastVariant;
  message: string;
  /** Auto-dismiss timeout in ms. Defaults: success/info=5000, error=7000. */
  durationMs?: number;
  /** Whether the user can close via the X button. Default true. */
  dismissible?: boolean;
  createdAt: number;
}

export type ToastInput = Omit<Toast, 'id' | 'createdAt'>;
export type ToastOptions = Partial<Omit<Toast, 'id' | 'createdAt' | 'message' | 'variant'>>;

type Listener = (toast: ToastInput) => void;

let listeners: ReadonlyArray<Listener> = [];

export const toastBus = {
  subscribe(listener: Listener): () => void {
    listeners = [...listeners, listener];
    return () => {
      listeners = listeners.filter((l) => l !== listener);
    };
  },
  emit(payload: ToastInput): void {
    // Snapshot to guard against listeners mutating the array during dispatch.
    const snapshot = listeners;
    for (const listener of snapshot) {
      try {
        listener(payload);
      } catch (err) {
        // A misbehaving listener must not break the dispatch loop.
        console.warn('[toast]', err);
      }
    }
  },
};

export const toast = {
  success: (message: string, opts?: ToastOptions): void =>
    toastBus.emit({ variant: 'success', message, ...opts }),
  error: (message: string, opts?: ToastOptions): void =>
    toastBus.emit({ variant: 'error', message, ...opts }),
  info: (message: string, opts?: ToastOptions): void =>
    toastBus.emit({ variant: 'info', message, ...opts }),
};

/** Normalize any thrown value into a human-readable string. */
export function formatError(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
