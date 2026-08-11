import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ArrowRight, ChevronRight, ClipboardCheck, Ban, AlertTriangle, CheckCircle2 } from 'lucide-react';
import type { InboxItem, InboxItemType } from '@/lib/inbox-model';
import { Card, CardHeader, CardTitle, CardAction, ActorAvatar, Empty } from '@/components/mds';
import { timeAgo } from '@/lib/format';

/**
 * NeedsAttentionList — the "需要你處理" list under the Home health summary
 * (W2-1). Unlike the customizable `NeedsMeRow` widget (which stays silent
 * when empty — a deliberate WP1.5 call for a *secondary* card), this is the
 * FIXED, always-rendered surface the health line's M/K counts point at, so
 * an empty queue must say so calmly rather than leave a blank gap (spec:
 * "空清單顯示平靜的空狀態，不是空白").
 *
 * Row navigation differs by type, per spec: approvals and needs_human tasks
 * ("等你決定") open the unified Inbox — deep-linked to the specific row via
 * `?item=<id>` (W2-5 H5) so it's already selected and scrolled into view,
 * not just "somewhere in the list"; undelivered channel sends ("沒送出
 * 去") open Channels management — there is nothing to decide about a failed
 * send inside the Inbox itself, the fix lives in channel config.
 *
 * Read-only: it never mutates anything; acting happens on the destination
 * page.
 */
const TYPE_ICON: Partial<Record<InboxItemType, React.ComponentType<{ className?: string }>>> = {
  approval: ClipboardCheck,
  blocked: Ban,
  failed_run: AlertTriangle,
};

function rowHref(item: InboxItem): string {
  if (item.type === 'failed_run') return '/manage/channels';
  return `/inbox?item=${encodeURIComponent(item.id)}`;
}

export interface NeedsAttentionListProps {
  /** Top slice already sorted by urgency (caller passes the first few). */
  items: readonly InboxItem[];
  /** Full count, for the "查看全部" affordance and the overflow hint. */
  total: number;
}

export function NeedsAttentionList({ items, total }: NeedsAttentionListProps) {
  const intl = useIntl();
  const overflow = total - items.length;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{intl.formatMessage({ id: 'home.overview.attention.title' })}</CardTitle>
        {items.length > 0 && (
          <CardAction>
            <Link
              to="/inbox"
              className="flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
            >
              {intl.formatMessage({ id: 'home.overview.attention.viewAll' })}
              <ArrowRight className="size-3" />
            </Link>
          </CardAction>
        )}
      </CardHeader>

      {items.length === 0 ? (
        <Empty
          icon={CheckCircle2}
          title={intl.formatMessage({ id: 'home.overview.attention.emptyTitle' })}
          description={intl.formatMessage({ id: 'home.overview.attention.emptyDescription' })}
          className="py-8"
        />
      ) : (
        <>
          <div className="px-2">
            {items.map((item) => {
              const Icon = TYPE_ICON[item.type] ?? ClipboardCheck;
              return (
                <Link
                  key={item.id}
                  to={rowHref(item)}
                  className="group/row flex h-11 items-center gap-3 rounded-md px-2 transition-colors hover:bg-surface-hover"
                >
                  {item.agentId ? (
                    <ActorAvatar actorType="agent" size="lg" name={item.agentId} />
                  ) : (
                    <span className="grid size-8 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground ring-1 ring-surface-border">
                      <Icon className="size-4" />
                    </span>
                  )}
                  <span className="min-w-0 flex-1 truncate text-sm text-foreground" title={item.title}>
                    {item.title}
                  </span>
                  <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                    {timeAgo(item.timestamp)}
                  </span>
                  <ChevronRight className="size-4 shrink-0 text-muted-foreground/50 transition-colors group-hover/row:text-muted-foreground" />
                </Link>
              );
            })}
          </div>
          {overflow > 0 && (
            <Link
              to="/inbox"
              className="block px-4 pb-3 text-xs text-muted-foreground transition-colors hover:text-foreground"
            >
              {intl.formatMessage({ id: 'home.overview.attention.more' }, { count: overflow })}
            </Link>
          )}
        </>
      )}
    </Card>
  );
}
