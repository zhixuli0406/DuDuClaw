import type { ComponentType, ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { CircleCheckBig, Wallet, BookOpen, Zap } from 'lucide-react';
import { CharacterAvatar } from '@/components/character';
import { useAgentsStore } from '@/stores/agents-store';
import { formatCents, formatXp } from '@/lib/format';
import { cn } from '@/lib/utils';
import type { DailyReport } from '@/lib/api-growth';

type StatTone = 'success' | 'neutral' | 'brand' | 'warning';

const toneClass: Record<StatTone, string> = {
  success: 'text-success',
  neutral: 'text-muted-foreground',
  brand: 'text-brand',
  warning: 'text-warning',
};

/** One KPI tile — icon + label + big value (spec §5.5 KPI row). */
function StatTile({
  icon: Icon,
  tone,
  label,
  value,
}: {
  icon: ComponentType<{ className?: string }>;
  tone: StatTone;
  label: string;
  value: ReactNode;
}) {
  return (
    <div className="rounded-lg border border-surface-border bg-card p-3">
      <div className="flex items-center gap-1.5">
        <Icon className={cn('size-4', toneClass[tone])} />
        <span className="text-xs font-medium text-muted-foreground">{label}</span>
      </div>
      <p className="mt-1.5 text-xl font-semibold tabular-nums text-foreground">{value}</p>
    </div>
  );
}

/**
 * DailyReportContent — the settlement figures for one day, reused by both the
 * once-per-day dialog (T10.3) and the `/growth` archive viewer (T10.2). Shows
 * completed tasks, spend, the most-active AI staffer (as a character avatar),
 * new knowledge pages, and the XP gained — with a localized note on how XP is
 * derived so the number never reads as more precise than it is.
 */
export function DailyReportContent({ report }: { report: DailyReport }) {
  const intl = useIntl();
  const agents = useAgentsStore((s) => s.agents);
  const topId = report.most_active_agent;
  const topAgent = topId ? agents.find((a) => a.name === topId) : undefined;
  const topName = topAgent?.display_name || topId || undefined;

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <StatTile
          icon={CircleCheckBig}
          tone="success"
          label={intl.formatMessage({ id: 'growth.report.tasks' })}
          value={report.tasks_completed}
        />
        <StatTile
          icon={Wallet}
          tone="neutral"
          label={intl.formatMessage({ id: 'growth.report.cost' })}
          value={formatCents(report.cost_cents)}
        />
        <StatTile
          icon={BookOpen}
          tone="brand"
          label={intl.formatMessage({ id: 'growth.report.newKnowledge' })}
          value={report.new_knowledge_pages}
        />
        <StatTile
          icon={Zap}
          tone="warning"
          label={intl.formatMessage({ id: 'growth.report.xpGained' })}
          value={`+${formatXp(report.xp_gained)}`}
        />
      </div>

      {/* Most-active staffer — a face, not a bare id (§3.2). */}
      <div className="flex items-center gap-3 rounded-lg border border-surface-border bg-card p-3">
        <span className="text-xs font-medium text-muted-foreground">
          {intl.formatMessage({ id: 'growth.report.activeAgent' })}
        </span>
        <div className="ml-auto flex items-center gap-2">
          {topName ? (
            <>
              <CharacterAvatar agentId={topId ?? topName} name={topName} size={28} />
              <span className="text-sm font-medium text-foreground">{topName}</span>
            </>
          ) : (
            <span className="text-sm text-muted-foreground">
              {intl.formatMessage({ id: 'growth.report.noAgent' })}
            </span>
          )}
        </div>
      </div>

      {/* Honesty annotation: how xp_gained was derived. Rendered from a
          localized string — the gateway's raw `xp_basis` note is an internal
          English implementation detail and must not surface in the UI (#6). */}
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        {intl.formatMessage({ id: 'growth.report.basis' })}
      </p>
    </div>
  );
}
