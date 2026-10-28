import type { ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Button } from '@/components/mds';

/**
 * The 交辦任務 page footer: a top border, an optional line on the left (who
 * runs it) and 取消 / submit on the right. On a phone the two stack and the
 * submit button takes the full width.
 */
export function AssignFooter({
  note,
  submitLabel,
  onCancel,
  onSubmit,
  canSubmit,
  submitting,
}: {
  note?: ReactNode;
  submitLabel: string;
  onCancel: () => void;
  onSubmit: () => void;
  canSubmit: boolean;
  submitting: boolean;
}) {
  const intl = useIntl();
  return (
    <div className="mt-6 flex flex-col gap-3 border-t border-surface-border pt-4 sm:flex-row sm:items-center sm:justify-between">
      <div className="min-w-0 text-sm text-muted-foreground">{note}</div>
      <div className="flex flex-col-reverse gap-2 sm:flex-row sm:shrink-0">
        <Button variant="outline" className="min-h-[44px] w-full sm:min-h-0 sm:w-auto" onClick={onCancel}>
          {intl.formatMessage({ id: 'common.cancel' })}
        </Button>
        <Button
          variant="brand"
          className="min-h-[44px] w-full sm:min-h-0 sm:w-auto"
          onClick={onSubmit}
          disabled={!canSubmit || submitting}
        >
          {submitting ? intl.formatMessage({ id: 'assign.submitting' }) : submitLabel}
        </Button>
      </div>
    </div>
  );
}
