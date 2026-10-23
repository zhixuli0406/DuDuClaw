import { ClipboardCheck } from 'lucide-react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import type { AssignedTaskRef } from '@/stores/chat-store';

/** The card under a reply that created a task (`/交辦` in WebChat): the
 *  task number and a link to the "assigned by me" view of the goals page,
 *  where its progress and result appear (WebChat gets no progress pushes). */
export function AssignedTaskCard({ task }: { task: AssignedTaskRef }) {
  const intl = useIntl();
  const short = task.id.slice(0, 8);
  return (
    <div className="mt-2 flex items-center justify-between gap-3 rounded-lg border border-surface-border bg-background/60 px-3 py-2">
      <div className="flex min-w-0 items-center gap-2 text-sm">
        <ClipboardCheck className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        <span className="truncate">{intl.formatMessage({ id: 'webchat.task.title' }, { id: short })}</span>
      </div>
      <Link
        to="/goals?assigned=me"
        className="shrink-0 text-xs font-medium text-primary underline-offset-2 hover:underline"
      >
        {intl.formatMessage({ id: 'webchat.task.open' })}
      </Link>
    </div>
  );
}
