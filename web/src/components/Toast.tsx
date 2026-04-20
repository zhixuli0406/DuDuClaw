import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { useIntl } from 'react-intl';
import { AlertCircle, CheckCircle, Info, X } from 'lucide-react';
import {
  toastBus,
  type Toast as ToastShape,
  type ToastInput,
  type ToastVariant,
} from '@/lib/toast';

interface ToastContextValue {
  show: (input: ToastInput) => string;
  dismiss: (id: string) => void;
}

const ToastContext = createContext<ToastContextValue | null>(null);

const MAX_TOASTS = 5;
const DEFAULT_DURATION_MS = 5000;
const ERROR_DURATION_MS = 7000;

function defaultDurationFor(variant: ToastVariant): number {
  return variant === 'error' ? ERROR_DURATION_MS : DEFAULT_DURATION_MS;
}

function prefersReducedMotion(): boolean {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') {
    return false;
  }
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function createId(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (g.crypto && typeof g.crypto.randomUUID === 'function') {
    return g.crypto.randomUUID();
  }
  return `toast-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ReadonlyArray<ToastShape>>([]);
  const timeoutsRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());

  const clearTimeoutFor = useCallback((id: string) => {
    const timers = timeoutsRef.current;
    const handle = timers.get(id);
    if (handle !== undefined) {
      clearTimeout(handle);
      timers.delete(id);
    }
  }, []);

  const dismiss = useCallback((id: string) => {
    clearTimeoutFor(id);
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, [clearTimeoutFor]);

  const show = useCallback(
    (input: ToastInput): string => {
      const id = createId();
      const createdAt = Date.now();
      const durationMs = input.durationMs ?? defaultDurationFor(input.variant);
      const dismissible = input.dismissible ?? true;
      const next: ToastShape = {
        id,
        variant: input.variant,
        message: input.message,
        durationMs,
        dismissible,
        createdAt,
      };

      setToasts((prev) => {
        // Max 5; drop oldest when full.
        const trimmed = prev.length >= MAX_TOASTS ? prev.slice(prev.length - (MAX_TOASTS - 1)) : prev;
        return [...trimmed, next];
      });

      if (durationMs > 0) {
        const handle = setTimeout(() => {
          timeoutsRef.current.delete(id);
          setToasts((prev) => prev.filter((t) => t.id !== id));
        }, durationMs);
        timeoutsRef.current.set(id, handle);
      }

      return id;
    },
    [],
  );

  // Bridge the module-scoped bus into the provider's `show`.
  useEffect(() => {
    const unsubscribe = toastBus.subscribe((payload) => {
      show(payload);
    });
    return unsubscribe;
  }, [show]);

  // Clean up any pending timers on unmount.
  useEffect(() => {
    const timers = timeoutsRef.current;
    return () => {
      for (const handle of timers.values()) {
        clearTimeout(handle);
      }
      timers.clear();
    };
  }, []);

  const contextValue = useMemo<ToastContextValue>(() => ({ show, dismiss }), [show, dismiss]);

  return (
    <ToastContext.Provider value={contextValue}>
      {children}
      <ToastViewport toasts={toasts} onDismiss={dismiss} />
    </ToastContext.Provider>
  );
}

export function useToast(): ToastContextValue {
  const ctx = useContext(ToastContext);
  if (!ctx) {
    throw new Error('useToast must be used within a <ToastProvider>');
  }
  return ctx;
}

interface ToastViewportProps {
  toasts: ReadonlyArray<ToastShape>;
  onDismiss: (id: string) => void;
}

function ToastViewport({ toasts, onDismiss }: ToastViewportProps) {
  if (toasts.length === 0) return null;
  return (
    <div
      className="pointer-events-none fixed bottom-4 right-4 z-[9999] flex max-w-sm flex-col gap-2"
      aria-label="Notifications"
    >
      {toasts.map((t) => (
        <ToastItem key={t.id} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

interface ToastItemProps {
  toast: ToastShape;
  onDismiss: (id: string) => void;
}

const VARIANT_STYLES: Record<ToastVariant, string> = {
  success:
    'border-emerald-200 bg-emerald-50 text-emerald-900 dark:border-emerald-800 dark:bg-emerald-900/20 dark:text-emerald-100',
  error:
    'border-rose-200 bg-rose-50 text-rose-900 dark:border-rose-800 dark:bg-rose-900/20 dark:text-rose-100',
  info: 'border-amber-200 bg-amber-50 text-amber-900 dark:border-amber-800 dark:bg-amber-900/20 dark:text-amber-100',
};

const ICON_STYLES: Record<ToastVariant, string> = {
  success: 'text-emerald-500 dark:text-emerald-400',
  error: 'text-rose-500 dark:text-rose-400',
  info: 'text-amber-500 dark:text-amber-400',
};

function ToastIcon({ variant }: { variant: ToastVariant }) {
  const className = `h-5 w-5 shrink-0 ${ICON_STYLES[variant]}`;
  if (variant === 'success') return <CheckCircle aria-hidden="true" className={className} />;
  if (variant === 'error') return <AlertCircle aria-hidden="true" className={className} />;
  return <Info aria-hidden="true" className={className} />;
}

function ToastItem({ toast, onDismiss }: ToastItemProps) {
  const intl = useIntl();
  const [entered, setEntered] = useState(false);
  const reducedMotion = useMemo(prefersReducedMotion, []);

  // Trigger slide-in after mount so the transition actually animates.
  useEffect(() => {
    if (reducedMotion) {
      setEntered(true);
      return;
    }
    const handle = requestAnimationFrame(() => setEntered(true));
    return () => cancelAnimationFrame(handle);
  }, [reducedMotion]);

  const role = toast.variant === 'error' ? 'alert' : 'status';
  const ariaLive = toast.variant === 'error' ? 'assertive' : 'polite';

  const baseClasses =
    'pointer-events-auto flex items-start gap-3 rounded-xl border px-4 py-3 shadow-lg backdrop-blur-sm';
  const variantClasses = VARIANT_STYLES[toast.variant];
  const motionClasses = reducedMotion
    ? ''
    : `transition-all duration-200 ease-out ${entered ? 'translate-x-0 opacity-100' : 'translate-x-4 opacity-0'}`;

  // Fallback label in case i18n isn't ready yet.
  let closeLabel = 'Close';
  try {
    closeLabel = intl.formatMessage({ id: 'toast.close' });
  } catch {
    // Fall back to the hardcoded label.
  }

  return (
    <div
      role={role}
      aria-live={ariaLive}
      aria-atomic="true"
      className={`${baseClasses} ${variantClasses} ${motionClasses}`.trim()}
    >
      <ToastIcon variant={toast.variant} />
      <p className="min-w-0 flex-1 whitespace-pre-line break-words text-sm leading-relaxed">
        {toast.message}
      </p>
      {toast.dismissible && (
        <button
          type="button"
          onClick={() => onDismiss(toast.id)}
          aria-label={closeLabel}
          className="shrink-0 rounded-md p-1 text-current/70 transition-colors hover:text-current hover:bg-black/5 dark:hover:bg-white/10 focus:outline-none focus:ring-2 focus:ring-current/40"
        >
          <X aria-hidden="true" className="h-4 w-4" />
        </button>
      )}
    </div>
  );
}
