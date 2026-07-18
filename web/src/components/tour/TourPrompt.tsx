import { useIntl } from 'react-intl';
import { useTourStore } from '@/stores/tour-store';
import { Button } from '@/components/mds';
import { PawPrint, X } from 'lucide-react';

/**
 * TourPrompt — the gentle "要不要帶你逛一圈?" card shown bottom-right right after
 * the first agent is created. Accept starts the guided tour; "稍後" marks it
 * skipped so it won't nag again. Mounted once in MainLayout.
 *
 * Anchored bottom-right (`bottom-28 right-5`) so it clears the mobile bottom
 * nav and leaves room for the DuDu character companion (V9) that will share
 * this corner.
 */
export function TourPrompt() {
  const intl = useIntl();
  const status = useTourStore((s) => s.status);
  const promptPending = useTourStore((s) => s.promptPending);
  const start = useTourStore((s) => s.start);
  const dismiss = useTourStore((s) => s.dismissPrompt);

  if (status !== 'unset' || !promptPending) return null;

  return (
    <div className="fixed bottom-28 right-5 z-[90] w-80 max-w-[calc(100vw-2.5rem)]">
      <div className="space-y-3 rounded-xl border border-surface-border bg-surface p-4 shadow-[var(--menu-shadow)]">
        <div className="flex items-start gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-brand/10 text-brand ring-1 ring-inset ring-brand/20">
            <PawPrint className="h-5 w-5" />
          </span>
          <div className="min-w-0 flex-1">
            <h3 className="text-sm font-semibold text-foreground">
              {intl.formatMessage({ id: 'tour.prompt.title' })}
            </h3>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {intl.formatMessage({ id: 'tour.prompt.body' })}
            </p>
          </div>
          <button
            onClick={dismiss}
            className="rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            aria-label={intl.formatMessage({ id: 'tour.prompt.decline' })}
          >
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={dismiss}>
            {intl.formatMessage({ id: 'tour.prompt.decline' })}
          </Button>
          <Button variant="default" size="sm" onClick={start}>
            {intl.formatMessage({ id: 'tour.prompt.accept' })}
          </Button>
        </div>
      </div>
    </div>
  );
}
