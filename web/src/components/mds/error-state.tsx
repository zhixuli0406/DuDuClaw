import {
  useCallback,
  useMemo,
  useState,
  type ComponentType,
  type ReactNode,
} from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, ChevronRight, RotateCw } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  classifyError,
  errorReasonKey,
  sanitizeErrorDetail,
} from '@/lib/error-message';
import { Button } from './button';

/**
 * WCAG AA contrast fix (light theme only). Over the inline banner's tinted
 * `bg-destructive/5`, the shared `--destructive` (L 0.577) headline and
 * `--muted-foreground` (L 0.552) body land at ~3.8:1 — below the 4.5:1 floor
 * for normal text. Darkening the *shared* tokens would ripple everywhere, so
 * these scoped OKLCH values darken only ErrorState's text and only in light
 * mode; `dark:` restores the tokens, whose dark values already clear 4.5:1.
 * Same hue/chroma as the tokens (still reads as the destructive red / neutral
 * grey), just a lower L. Kept as class constants so both variants share one fix.
 */
const ERROR_TITLE_COLOR =
  'text-[oklch(0.505_0.21_27.325)] dark:text-destructive';
const ERROR_BODY_COLOR =
  'text-[oklch(0.48_0.016_285.938)] dark:text-muted-foreground';

/**
 * ErrorState — the one way this dashboard reports a failure that the user can
 * still act on (rubric P05).
 *
 * Three rules it exists to enforce, all of them audit findings:
 *
 *  1. **Persistent, not a toast.** A 7-second toast that leaves the page in an
 *     empty state tells the user "you have no data" — the opposite of the truth.
 *  2. **Plain language first, raw string never.** The headline and the reason
 *     come from `errorState.*` messages; the thrown value is sanitized and
 *     tucked behind a "technical details" disclosure.
 *  3. **A way out.** `onRetry` renders a retry button, so a failed load is
 *     recoverable without a page reload.
 *
 * `variant="page"` fills an empty region (list/detail body); `variant="inline"`
 * is the compact banner for a failure inside a populated view (a panel that
 * couldn't refresh, a mutation that didn't land).
 */
export interface ErrorStateProps {
  /** Plain-language headline. Defaults to `errorState.title`. */
  title?: ReactNode;
  /**
   * Plain-language explanation. Defaults to the reason classified from
   * `error` (`errorState.reason.*`).
   */
  description?: ReactNode;
  /**
   * The thrown value. Used for the default description and — sanitized — for
   * the technical-details disclosure. Never rendered verbatim.
   */
  error?: unknown;
  /** Renders the retry button when supplied. */
  onRetry?: () => void;
  /** Overrides the retry button label (`errorState.retry`). */
  retryLabel?: ReactNode;
  /** Extra action rendered next to retry (e.g. "go to settings"). */
  action?: ReactNode;
  icon?: ComponentType<{ className?: string }>;
  variant?: 'page' | 'inline';
  className?: string;
}

export function ErrorState({
  title,
  description,
  error,
  onRetry,
  retryLabel,
  action,
  icon: Icon = AlertTriangle,
  variant = 'page',
  className,
}: ErrorStateProps) {
  const intl = useIntl();
  const [detailOpen, setDetailOpen] = useState(false);

  const detail = useMemo(() => sanitizeErrorDetail(error), [error]);
  const reason = useMemo(
    () => intl.formatMessage({ id: errorReasonKey(classifyError(error)) }),
    [intl, error]
  );

  const headline = title ?? intl.formatMessage({ id: 'errorState.title' });
  const body = description ?? reason;

  const retryButton = onRetry ? (
    <Button variant="outline" size="sm" onClick={onRetry}>
      <RotateCw />
      {retryLabel ?? intl.formatMessage({ id: 'errorState.retry' })}
    </Button>
  ) : null;

  const disclosure = detail ? (
    <div
      className={cn(
        'w-full text-left',
        variant === 'page' ? 'mt-4 max-w-md' : 'mt-1.5'
      )}
    >
      <button
        type="button"
        onClick={() => setDetailOpen((open) => !open)}
        aria-expanded={detailOpen}
        className="inline-flex min-h-6 items-center gap-1 rounded text-xs text-muted-foreground outline-none focus-visible:ring-3 focus-visible:ring-ring/50 hover:text-foreground"
      >
        <ChevronRight
          className={cn(
            'size-3 transition-transform',
            detailOpen && 'rotate-90'
          )}
        />
        {intl.formatMessage({
          id: detailOpen ? 'errorState.details.hide' : 'errorState.details.show',
        })}
      </button>
      {detailOpen && (
        <pre
          data-slot="error-state-detail"
          className="mt-1.5 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded-md bg-muted px-2 py-1.5 font-mono text-[0.7rem] leading-relaxed text-muted-foreground"
        >
          {detail}
        </pre>
      )}
    </div>
  ) : null;

  if (variant === 'inline') {
    return (
      <div
        role="alert"
        data-slot="error-state"
        data-variant="inline"
        className={cn(
          'flex items-start gap-2.5 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2.5',
          className
        )}
      >
        <Icon className="mt-0.5 size-4 shrink-0 text-destructive" />
        <div className="min-w-0 flex-1">
          <p className={cn('text-sm font-medium', ERROR_TITLE_COLOR)}>{headline}</p>
          <p className={cn('mt-0.5 text-sm', ERROR_BODY_COLOR)}>{body}</p>
          {disclosure}
        </div>
        {(retryButton || action) && (
          <div className="flex shrink-0 items-center gap-2">
            {retryButton}
            {action}
          </div>
        )}
      </div>
    );
  }

  return (
    <div
      role="alert"
      data-slot="error-state"
      data-variant="page"
      className={cn(
        'flex flex-col items-center justify-center px-5 py-16 text-center',
        className
      )}
    >
      <div className="mb-4 flex size-12 items-center justify-center rounded-full bg-destructive/10">
        <Icon className="size-6 text-destructive" />
      </div>
      <p className={cn('text-sm font-medium', ERROR_TITLE_COLOR)}>{headline}</p>
      <p className={cn('mt-1 max-w-md text-sm', ERROR_BODY_COLOR)}>{body}</p>
      {(retryButton || action) && (
        <div className="mt-4 flex flex-wrap items-center justify-center gap-2">
          {retryButton}
          {action}
        </div>
      )}
      {disclosure}
    </div>
  );
}

/**
 * Plain-language one-liner for a thrown value, for surfaces that only have room
 * for a sentence (toasts, inline field errors). Same classification as
 * `ErrorState`, so a failure reads identically wherever it surfaces — and the
 * raw string still never reaches the user.
 */
export function useErrorMessage(): (error: unknown) => string {
  const intl = useIntl();
  return useCallback(
    (error: unknown) =>
      intl.formatMessage({ id: errorReasonKey(classifyError(error)) }),
    [intl]
  );
}
