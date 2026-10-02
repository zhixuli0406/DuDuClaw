import { useIntl } from 'react-intl';
import { Info, RotateCw } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/mds';

/**
 * L2: a fanned-out list (one request per bound AI employee) came back
 * PARTIAL — some employees answered, at least one failed. The data that
 * arrived is still shown; this is the one quiet, non-destructive line that
 * says the view is incomplete. A total failure is an `ErrorState`, not this.
 */
export function PartialLoadNotice({ onRetry, className }: { onRetry?: () => void; className?: string }) {
  const intl = useIntl();
  return (
    <div
      role="status"
      className={cn('flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground', className)}
    >
      <Info className="size-3.5 shrink-0" aria-hidden="true" />
      <span>{intl.formatMessage({ id: 'partialLoad.notice' })}</span>
      {onRetry && (
        <Button variant="ghost" size="xs" onClick={onRetry}>
          <RotateCw className="size-3" aria-hidden="true" />
          {intl.formatMessage({ id: 'errorState.retry' })}
        </Button>
      )}
    </div>
  );
}
