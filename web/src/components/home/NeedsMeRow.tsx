import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import {
  ArrowRight,
  ChevronRight,
  ClipboardCheck,
  Package,
  Ban,
  Wallet,
  AlertTriangle,
  GitBranch,
} from 'lucide-react';
import type { InboxItem, InboxItemType } from '@/lib/inbox-model';
import {
  Card,
  CardHeader,
  CardTitle,
  CardAction,
  ActorAvatar,
} from '@/components/mds';
import { timeAgo } from '@/lib/format';

/**
 * NeedsMeRow — the "需要我" strip on Home (WP1.5, spec §5.5 report style). Shows
 * the top slice of the unified inbox (approvals / blocked / budget / failed-run,
 * already merged + urgency-sorted by the page) as slim rows, each with an icon,
 * title, relative time and a jump affordance into `/inbox`. Read-only: it never
 * mutates the inbox; acting happens on `/inbox`.
 *
 * Empty → renders nothing (WP1.5 拍板: "空則不顯示"), keeping the home canvas quiet
 * when there is nothing waiting on the user.
 */
const TYPE_ICON: Record<InboxItemType, React.ComponentType<{ className?: string }>> = {
  approval: ClipboardCheck,
  install: Package,
  decision: GitBranch,
  blocked: Ban,
  budget: Wallet,
  failed_run: AlertTriangle,
};

export interface NeedsMeRowProps {
  /** Top slice of the merged inbox (page passes the first few). */
  items: readonly InboxItem[];
  /** Full count for the "全部 →" affordance context. */
  total: number;
}

export function NeedsMeRow({ items }: NeedsMeRowProps) {
  const intl = useIntl();

  // 拍板: empty state is silence, not a friendly card.
  if (items.length === 0) return null;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{intl.formatMessage({ id: 'home.inbox.title' })}</CardTitle>
        <CardAction>
          <Link
            to="/inbox"
            className="flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground"
          >
            {intl.formatMessage({ id: 'home.inbox.viewAll' })}
            <ArrowRight className="size-3" />
          </Link>
        </CardAction>
      </CardHeader>
      <div className="px-2">
        {items.map((p) => {
          const Icon = TYPE_ICON[p.type];
          return (
            <Link
              key={p.id}
              to="/inbox"
              className="group/row flex h-11 items-center gap-3 rounded-md px-2 transition-colors hover:bg-surface-hover"
            >
              {p.agentId ? (
                <ActorAvatar actorType="agent" size="lg" name={p.agentId} />
              ) : (
                <span className="grid size-8 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground ring-1 ring-surface-border">
                  <Icon className="size-4" />
                </span>
              )}
              <span
                className="min-w-0 flex-1 truncate text-sm text-foreground"
                title={p.title}
              >
                {p.title}
              </span>
              <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                {timeAgo(p.timestamp)}
              </span>
              <ChevronRight className="size-4 shrink-0 text-muted-foreground/50 transition-colors group-hover/row:text-muted-foreground" />
            </Link>
          );
        })}
      </div>
    </Card>
  );
}
