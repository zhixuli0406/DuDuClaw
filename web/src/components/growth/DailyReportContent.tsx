import { useIntl } from 'react-intl';
import { CircleCheckBig, Wallet, BookOpen, Zap } from 'lucide-react';
import { StatCard, Mono } from '@/components/ui';
import { CharacterAvatar } from '@/components/character';
import { useAgentsStore } from '@/stores/agents-store';
import { formatCents, formatXp } from '@/lib/format';
import type { DailyReport } from '@/lib/api-growth';

/**
 * DailyReportContent — the settlement figures for one day, reused by both the
 * once-per-day dialog (T10.3) and the `/growth` archive viewer (T10.2). Shows
 * completed tasks, spend, the most-active AI staffer (as a character avatar),
 * new knowledge pages, and the XP gained — always with the gateway's honest
 * `xp_basis` note so the number never reads as more precise than it is.
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
        <StatCard
          icon={CircleCheckBig}
          tone="success"
          label={intl.formatMessage({ id: 'growth.report.tasks' })}
          value={report.tasks_completed}
        />
        <StatCard
          icon={Wallet}
          tone="neutral"
          label={intl.formatMessage({ id: 'growth.report.cost' })}
          value={formatCents(report.cost_cents)}
        />
        <StatCard
          icon={BookOpen}
          tone="accent"
          label={intl.formatMessage({ id: 'growth.report.newKnowledge' })}
          value={report.new_knowledge_pages}
        />
        <StatCard
          icon={Zap}
          tone="warning"
          label={intl.formatMessage({ id: 'growth.report.xpGained' })}
          value={`+${formatXp(report.xp_gained)}`}
        />
      </div>

      {/* Most-active staffer — a face, not a bare id (§3.2). */}
      <div className="panel flex items-center gap-3 p-3">
        <span className="text-xs font-medium text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'growth.report.activeAgent' })}
        </span>
        <div className="ml-auto flex items-center gap-2">
          {topName ? (
            <>
              <CharacterAvatar agentId={topId ?? topName} name={topName} size={28} />
              <span className="text-sm font-medium text-stone-800 dark:text-stone-200">{topName}</span>
            </>
          ) : (
            <span className="text-sm text-stone-400 dark:text-stone-500">
              {intl.formatMessage({ id: 'growth.report.noAgent' })}
            </span>
          )}
        </div>
      </div>

      {/* Honesty annotation: how xp_gained was derived. */}
      <p className="text-[11px] leading-relaxed text-stone-400 dark:text-stone-500">
        {intl.formatMessage({ id: 'growth.report.basis' })} <Mono>{report.xp_basis}</Mono>
      </p>
    </div>
  );
}
