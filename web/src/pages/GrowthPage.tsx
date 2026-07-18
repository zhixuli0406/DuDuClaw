import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { HelpCircle, TrendingUp, Bot, CircleCheckBig, BookOpen, Wand2, Repeat, Wand } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  PageHeader,
  Card,
  CardHeader,
  CardTitle,
  CardAction,
  CardContent,
  Empty,
  Skeleton,
} from '@/components/mds';
import { useGrowthStore } from '@/stores/growth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { growthApi, type DailyReport, type Achievement } from '@/lib/api-growth';
import { formatXp } from '@/lib/format';
import { ACHIEVEMENT_DEFS } from '@/components/growth/achievements-def';
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

/** Build the last N local calendar days as `YYYY-MM-DD`, most-recent first,
 *  including today (`i` starts at 0). Today was previously excluded, so the
 *  archive never offered a tab for the current day (#7); the report for today
 *  may be empty until the gateway settles it, which the viewer shows honestly. */
function recentDays(n: number): string[] {
  const out: string[] = [];
  const base = new Date();
  for (let i = 0; i < n; i++) {
    const d = new Date(base);
    d.setDate(base.getDate() - i);
    out.push(localDayKey(d));
  }
  return out;
}

/**
 * One cell on the achievement wall (spec §5.5 / T10.2). Unlocked = full color;
 * locked / unavailable = `opacity-40 grayscale`. `available === false` is an
 * honest state (backend can't evaluate yet), surfaced with the reason on hover
 * — never a fake 0% lock (§6.3 honesty rule).
 */
function AchievementWallCell({ ach }: { ach: Achievement }) {
  const intl = useIntl();
  const def = ACHIEVEMENT_DEFS[ach.id];
  const name = def ? intl.formatMessage({ id: def.nameId }) : ach.id;
  const desc = def ? intl.formatMessage({ id: def.descId }) : undefined;
  const Icon = def?.icon ?? HelpCircle;
  const dimmed = !ach.unlocked || !ach.available;

  return (
    <div
      className={cn(
        'flex flex-col items-center gap-2 rounded-xl border border-surface-border bg-surface p-4 text-center transition-opacity',
        dimmed && 'opacity-40 grayscale',
      )}
      title={!ach.available ? ach.unavailable_reason ?? undefined : desc}
      aria-label={name}
    >
      <span
        className={cn(
          'grid size-10 shrink-0 place-items-center rounded-full',
          ach.unlocked ? 'bg-chart-1/15 text-chart-1' : 'bg-muted text-muted-foreground',
        )}
      >
        <Icon className="size-5" aria-hidden="true" />
      </span>
      <p className="line-clamp-2 text-xs font-medium leading-tight text-foreground">{name}</p>
    </div>
  );
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
    <Card>
      <CardHeader>
        <CardTitle>{intl.formatMessage({ id: 'growth.report.archive.title' })}</CardTitle>
      </CardHeader>
      <CardContent>
        <div className="mb-4 flex flex-wrap gap-1.5">
          {days.map((d) => (
            <button
              key={d}
              type="button"
              onClick={() => setDate(d)}
              className={cn(
                'rounded-md px-2.5 py-1 font-mono text-xs tabular-nums transition-colors',
                d === date
                  ? 'bg-accent text-accent-foreground'
                  : 'text-muted-foreground hover:bg-muted hover:text-foreground',
              )}
            >
              {d.slice(5)}
            </button>
          ))}
        </div>
        {loading ? (
          <Skeleton className="h-40 w-full" />
        ) : failed || !report ? (
          <p className="py-6 text-center text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'growth.report.empty' })}
          </p>
        ) : (
          <DailyReportContent report={report} />
        )}
      </CardContent>
    </Card>
  );
}

/**
 * GrowthPage (`/growth`) — the company's level, XP, achievement wall, and daily
 * report archive (spec §5.5, T10.2), re-skinned onto MDS. All figures are
 * read-only projections of real gateway data via `growth.snapshot` /
 * `growth.daily_report` — the front end never invents XP or unlock state.
 */
export function GrowthPage() {
  const intl = useIntl();
  const snapshot = useGrowthStore((s) => s.snapshot);
  const loaded = useGrowthStore((s) => s.loaded);
  const authed = useConnectionStore((s) => s.state === 'authenticated');

  const unlockedCount = snapshot?.achievements.filter((a) => a.unlocked).length ?? 0;
  const totalAchievements = snapshot?.achievements.length ?? 0;
  const xpPct =
    snapshot && snapshot.xp_for_next_level > 0
      ? Math.min(100, (snapshot.xp_into_level / snapshot.xp_for_next_level) * 100)
      : 0;

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <PageHeader hideTrigger>
        <TrendingUp className="size-4 shrink-0 text-muted-foreground" />
        <h1 className="truncate text-sm font-medium">{intl.formatMessage({ id: 'nav.growth' })}</h1>
      </PageHeader>

      <div className="mx-auto w-full max-w-6xl space-y-5 p-6">
        {/* Level card — big level, XP bar, into/next figures, and the six facts. */}
        {!loaded ? (
          <Skeleton className="h-44 w-full" />
        ) : !snapshot ? (
          <Card>
            <CardContent>
              <Empty
                icon={TrendingUp}
                title={intl.formatMessage({ id: 'growth.unavailable.title' })}
                description={intl.formatMessage({ id: 'growth.unavailable.desc' })}
              />
            </CardContent>
          </Card>
        ) : (
          <>
            <Card>
              <CardContent className="space-y-5">
                <div className="flex flex-col gap-5 sm:flex-row sm:items-center">
                  <div className="flex items-baseline gap-2">
                    <span className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
                      Lv
                    </span>
                    <span className="text-5xl font-semibold tabular-nums text-foreground">
                      {snapshot.level}
                    </span>
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="h-2 overflow-hidden rounded-full bg-muted">
                      <div
                        className="h-full rounded-full bg-chart-1 transition-all duration-500"
                        style={{ width: `${xpPct}%` }}
                      />
                    </div>
                    <p className="mt-1.5 font-mono text-xs tabular-nums text-muted-foreground">
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
                <div className="grid grid-cols-2 gap-3 border-t border-surface-border pt-4 sm:grid-cols-3">
                  {FACT_ROWS.map(({ key, icon: Icon, labelId }) => (
                    <div key={key} className="flex items-center gap-2.5">
                      <span className="grid size-8 shrink-0 place-items-center rounded-lg bg-muted text-muted-foreground">
                        <Icon className="size-4" />
                      </span>
                      <div className="min-w-0">
                        <p className="text-lg font-semibold leading-none tabular-nums text-foreground">
                          {snapshot.facts[key]}
                        </p>
                        <p className="mt-0.5 truncate text-[11px] text-muted-foreground">
                          {intl.formatMessage({ id: labelId })}
                        </p>
                      </div>
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>

            {/* Achievement wall. */}
            <Card>
              <CardHeader>
                <CardTitle>{intl.formatMessage({ id: 'growth.achievements.title' })}</CardTitle>
                <CardAction>
                  <span className="font-mono text-xs tabular-nums text-muted-foreground">
                    {intl.formatMessage(
                      { id: 'growth.achievements.unlockedCount' },
                      { unlocked: unlockedCount, total: totalAchievements },
                    )}
                  </span>
                </CardAction>
              </CardHeader>
              <CardContent>
                <div className="grid grid-cols-3 gap-4 sm:grid-cols-4 lg:grid-cols-6">
                  {snapshot.achievements.map((ach) => (
                    <AchievementWallCell key={ach.id} ach={ach} />
                  ))}
                </div>
              </CardContent>
            </Card>
          </>
        )}

        {/* Daily report archive (last 7 days). */}
        {authed && <DailyReportArchive />}
      </div>
    </div>
  );
}
