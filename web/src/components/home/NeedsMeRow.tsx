import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ArrowRight, ClipboardCheck, Ban, Wallet, AlertTriangle, GitBranch } from 'lucide-react';
import type { InboxItem, InboxItemType } from '@/lib/inbox-model';
import { CharacterAvatar, Card, Mono, DuDu } from '@/components/ui';
import { timeAgo } from '@/lib/format';

/**
 * NeedsMeRow — the "需要我" strip on Home (V3-T3.2). Shows the top 3 items of the
 * unified inbox (approvals / blocked / budget / failed-run mixed stream, already
 * merged + urgency-sorted by the page via `inbox-model.sortInbox`) with a
 * "全部 →" jump. Read-only: it never mutates the inbox; acting happens on /inbox.
 *
 * Empty state is the friendly "nothing needs you" copy with a small happy DuDu
 * in the `data-dudu-slot="home-clear"` anchor (V9 / §7.3).
 */
const TYPE_ICON: Record<InboxItemType, React.ComponentType<{ className?: string }>> = {
  approval: ClipboardCheck,
  decision: GitBranch,
  blocked: Ban,
  budget: Wallet,
  failed_run: AlertTriangle,
};

export interface NeedsMeRowProps {
  /** Top slice of the merged inbox (page passes the first 3). */
  items: readonly InboxItem[];
  /** Full count for the "全部 →" affordance context. */
  total: number;
}

export function NeedsMeRow({ items }: NeedsMeRowProps) {
  const intl = useIntl();

  return (
    <Card
      title={intl.formatMessage({ id: 'home.inbox.title' })}
      actions={
        <Link
          to="/inbox"
          className="flex items-center gap-1 text-xs text-stone-500 transition-colors hover:text-amber-600 dark:text-stone-400 dark:hover:text-amber-400"
        >
          {intl.formatMessage({ id: 'home.inbox.viewAll' })}
          <ArrowRight className="h-3 w-3" />
        </Link>
      }
    >
      {items.length === 0 ? (
        <div
          data-dudu-slot="home-clear"
          className="flex flex-col items-center gap-2 py-6 text-center"
        >
          <DuDu face="happy" size="sm" />
          <p className="text-sm text-stone-500 dark:text-stone-400">
            {intl.formatMessage({ id: 'home.inbox.empty' })}
          </p>
        </div>
      ) : (
        <div className="space-y-1">
          {items.map((p) => {
            const Icon = TYPE_ICON[p.type];
            return (
              <Link
                key={p.id}
                to="/inbox"
                className="flex items-center gap-3 rounded-lg px-2 py-2 transition-colors hover:bg-stone-500/8 dark:hover:bg-white/5"
              >
                {p.agentId ? (
                  <CharacterAvatar agentId={p.agentId} size={28} className="shrink-0" />
                ) : (
                  <span className="grid h-7 w-7 shrink-0 place-items-center rounded-lg bg-stone-500/10 text-stone-500 dark:bg-white/5 dark:text-stone-400">
                    <Icon className="h-4 w-4" />
                  </span>
                )}
                <span
                  className="min-w-0 flex-1 truncate text-sm text-stone-800 dark:text-stone-100"
                  title={p.title}
                >
                  {p.title}
                </span>
                <Mono className="shrink-0">{timeAgo(p.timestamp)}</Mono>
              </Link>
            );
          })}
        </div>
      )}
    </Card>
  );
}
