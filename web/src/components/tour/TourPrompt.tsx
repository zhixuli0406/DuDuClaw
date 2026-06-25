import { useIntl } from 'react-intl';
import { useTourStore } from '@/stores/tour-store';
import { Button } from '@/components/ui';
import { PawPrint, X } from 'lucide-react';

/**
 * TourPrompt — the gentle "要不要帶你逛一圈?" card shown bottom-right right after
 * the first agent is created. Accept starts the guided tour; "稍後" marks it
 * skipped so it won't nag again. Mounted once in MainLayout.
 */
export function TourPrompt() {
  const intl = useIntl();
  const status = useTourStore((s) => s.status);
  const promptPending = useTourStore((s) => s.promptPending);
  const start = useTourStore((s) => s.start);
  const dismiss = useTourStore((s) => s.dismissPrompt);

  if (status !== 'unset' || !promptPending) return null;

  return (
    <div className="fixed bottom-5 right-5 z-[90] w-80 max-w-[calc(100vw-2.5rem)]">
      <div className="panel space-y-3 p-4 shadow-xl">
        <div className="flex items-start gap-3">
          <span className="grid h-9 w-9 shrink-0 place-items-center rounded-xl bg-amber-500/12 text-amber-600 ring-1 ring-inset ring-amber-500/20 dark:bg-amber-400/10 dark:text-amber-400">
            <PawPrint className="h-5 w-5" />
          </span>
          <div className="min-w-0 flex-1">
            <h3 className="text-sm font-semibold text-stone-900 dark:text-stone-50">
              {intl.formatMessage({ id: 'tour.prompt.title' })}
            </h3>
            <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'tour.prompt.body' })}
            </p>
          </div>
          <button
            onClick={dismiss}
            className="rounded p-1 text-stone-400 transition-colors hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
            aria-label={intl.formatMessage({ id: 'tour.prompt.decline' })}
          >
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={dismiss}>
            {intl.formatMessage({ id: 'tour.prompt.decline' })}
          </Button>
          <Button variant="primary" size="sm" onClick={start}>
            {intl.formatMessage({ id: 'tour.prompt.accept' })}
          </Button>
        </div>
      </div>
    </div>
  );
}
