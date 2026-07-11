import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Trophy, Bot, CircleCheckBig, BookOpen, Wand2, Repeat, Wand } from 'lucide-react';
import { Page, PageHeader, Card, XpBar, EmptyState, Skeleton } from '@/components/ui';
import { useGrowthStore } from '@/stores/growth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { growthApi, type DailyReport } from '@/lib/api-growth';
import { formatXp } from '@/lib/format';
import { AchievementCell } from '@/components/growth/AchievementCell';
import { DailyReportContent } from '@/components/growth/DailyReportContent';
import { localDayKey } from '@/components/growth/DailyReportCard';

/** The six facts the XP score is derived from, with their icons + i18n labels. */
const FACT_ROWS = [
  { key: 'agents_count', icon: Bot, labelId: 'growth.fact.agents' },
  { key: 'tasks_completed', icon: CircleCheckBig, labelId: 'growth.fact.tasks' },
  { key: 'knowledge_pages', icon: BookOpen, labelId: 'growth.fact.knowledge' },
  { key: 'skills_acquired', icon: Wand2, labelId: 'growth.fact.skills' },
  { key: 'routines_completed', icon: Repeat, labelId: 'growth.fact.routines' },
  { key: 'custom_skills_approved', icon: Wand, labelId: 'growth.fact.customSkills' },
] as const;

/** Build the last N local calendar days as `YYYY-MM-DD`, most-recent first. */
function recentDays(n: number): string[] {
  const out: string[] = [];
  const base = new Date();
  for (let i = 1; i <= n; i++) {
    const d = new Date(base);
    d.setDate(base.getDate() - i);
    out.push(localDayKey(d));
  }
  return out;
}

/** Archive viewer: pick one of the last 7 days and read its settlement card. */
function DailyReportArchive() {
  const intl = useIntl();
  const days = useMemo(() => recentDays(7), []);
  const [date, setDate] = useState<string>(days[0]);
  const [report, setReport] = useState<DailyReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setFailed(false);
    growthApi
      .dailyReport(date)
      .then((r) => {
        if (alive) {
          setReport(r);
          setLoading(false);
        }
      })
      .catch(() => {
        if (alive) {
          setFailed(true);
          setLoading(false);
        }
      });
    return () => {
      alive = false;
    };
  }, [date]);

  return (
    <Card title={intl.formatMessage({ id: 'growth.report.archive.title' })}>
      <div className="mb-4 flex flex-wrap gap-1.5">
        {days.map((d) => (
          <button
            key={d}
            type="button"
            onClick={() => setDate(d)}
            className={
              'rounded-lg px-2.5 py-1 font-mono text-xs tabular-nums transition-colors ' +
              (d === date
                ? 'bg-amber-500/15 text-amber-700 ring-1 ring-inset ring-amber-500/30 dark:text-amber-300'
                : 'text-stone-500 hover:bg-stone-500/10 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200')
            }
          >
            {d.slice(5)}
          </button>
        ))}
      </div>
      {loading ? (
        <Skeleton className="h-40 w-full" />
      ) : failed || !report ? (
        <p className="py-6 text-center text-sm text-stone-400 dark:text-stone-500">
          {intl.formatMessage({ id: 'growth.report.empty' })}
        </p>
      ) : (
        <DailyReportContent report={report} />
      )}
    </Card>
  );
}

/**
 * GrowthPage (`/growth`) — the company's level, XP, achievement wall, and daily
 * report archive (dashboard-redesign-v2 §6, T10.2). All figures are read-only
 * projections of real gateway data via `growth.snapshot` / `growth.daily_report`
 * — the front end never invents XP or unlock state. The snapshot itself is
 * polled and cached by `GrowthMount` (mounted shell-wide); this page just reads
 * the store.
 */
export function GrowthPage() {
  const intl = useIntl();
  const snapshot = useGrowthStore((s) => s.snapshot);
  const loaded = useGrowthStore((s) => s.loaded);
  const authed = useConnectionStore((s) => s.state === 'authenticated');

  const unlockedCount = snapshot?.achievements.filter((a) => a.unlocked).length ?? 0;
  const totalAchievements = snapshot?.achievements.length ?? 0;

  return (
    <Page>
      <PageHeader
        icon={Trophy}
        title={intl.formatMessage({ id: 'nav.growth' })}
        subtitle={intl.formatMessage({ id: 'growth.subtitle' })}
      />

      {/* Level card — big level, XP bar, into/next figures, and the six facts. */}
      {!loaded ? (
        <Skeleton className="h-44 w-full" />
      ) : !snapshot ? (
        <Card>
          <EmptyState
            icon={Trophy}
            title={intl.formatMessage({ id: 'growth.unavailable.title' })}
            hint={intl.formatMessage({ id: 'growth.unavailable.desc' })}
          />
        </Card>
      ) : (
        <>
          <Card>
            <div className="flex flex-col gap-5 sm:flex-row sm:items-center">
              <div className="flex items-baseline gap-2">
                <span className="text-xs font-semibold uppercase tracking-wider text-stone-400 dark:text-stone-500">
                  Lv
                </span>
                <span className="text-5xl font-bold tabular-nums text-stone-900 dark:text-stone-50">
                  {snapshot.level}
                </span>
              </div>
              <div className="min-w-0 flex-1">
                <XpBar xp={snapshot.xp} showLevel={false} />
                <p className="mt-1.5 font-mono text-xs tabular-nums text-stone-500 dark:text-stone-400">
                  {intl.formatMessage(
                    { id: 'growth.xp.into' },
                    {
                      into: formatXp(snapshot.xp_into_level),
                      span: formatXp(snapshot.xp_for_next_level),
                    },
                  )}
                </p>
              </div>
            </div>

            {/* Six-fact mini grid — the datable basis for the XP score. */}
            <div className="mt-5 grid grid-cols-2 gap-3 border-t border-stone-300/40 pt-4 sm:grid-cols-3 dark:border-white/8">
              {FACT_ROWS.map(({ key, icon: Icon, labelId }) => (
                <div key={key} className="flex items-center gap-2.5">
                  <span className="grid h-8 w-8 shrink-0 place-items-center rounded-lg bg-stone-500/10 text-stone-500 dark:text-stone-400">
                    <Icon className="h-4 w-4" />
                  </span>
                  <div className="min-w-0">
                    <p className="text-lg font-semibold tabular-nums leading-none text-stone-900 dark:text-stone-50">
                      {snapshot.facts[key]}
                    </p>
                    <p className="mt-0.5 truncate text-[11px] text-stone-500 dark:text-stone-400">
                      {intl.formatMessage({ id: labelId })}
                    </p>
                  </div>
                </div>
              ))}
            </div>
          </Card>

          {/* Achievement wall. */}
          <Card
            title={intl.formatMessage({ id: 'growth.achievements.title' })}
            actions={
              <span className="font-mono text-xs tabular-nums text-stone-500 dark:text-stone-400">
                {intl.formatMessage(
                  { id: 'growth.achievements.unlockedCount' },
                  { unlocked: unlockedCount, total: totalAchievements },
                )}
              </span>
            }
          >
            <div className="grid gap-3 sm:grid-cols-2">
              {snapshot.achievements.map((ach) => (
                <AchievementCell key={ach.id} ach={ach} />
              ))}
            </div>
          </Card>
        </>
      )}

      {/* Daily report archive (last 7 days). */}
      {authed && <DailyReportArchive />}
    </Page>
  );
}
