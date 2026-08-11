import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type ActivityType } from '@/lib/api';
import { Card, CardHeader, CardTitle } from '@/components/mds';
import {
  readLastVisit,
  writeLastVisit,
  shouldShowSinceLastVisit,
  summarizeSinceLastVisit,
  type SinceVisitSummary,
} from '@/lib/since-last-visit';

/** Enough headroom for a typical multi-day gap (weekend, short trip). A
 *  visitor away long enough to blow past this fetch window gets an
 *  undercount, not a crash or an unbounded query — a deliberate trade-off,
 *  not an oversight (see HealthOverview's handoff-note convention). */
const FETCH_LIMIT = 200;

/** i18n message id per tracked category — see `SINCE_VISIT_CATEGORIES` in
 *  `lib/since-last-visit.ts` for why only these six. */
const CATEGORY_MESSAGE_ID: Partial<Record<ActivityType, string>> = {
  task_completed: 'home.sinceVisit.taskCompleted',
  skill_learned: 'home.sinceVisit.skillLearned',
  evolution_triggered: 'home.sinceVisit.evolutionTriggered',
  memory_distilled: 'home.sinceVisit.memoryDistilled',
  wiki_written: 'home.sinceVisit.wikiWritten',
  agent_created: 'home.sinceVisit.agentCreated',
};

/**
 * SinceLastVisit — "你不在的時候" (W2-1, 02-ux-methodology.md §P3-2). Anchors
 * on the previous visit's localStorage stamp, fetches recent activity, and
 * renders a short recap sentence ("完成 12 件任務、學會 2 條新法則、1 次行
 * 為調整生效") instead of a scrollable log. Silent (renders nothing) on a
 * first visit, when the last visit was under 30 minutes ago, or when nothing
 * in the tracked categories happened — a below-the-fold recap earning its
 * place is more calm-tech-aligned than an empty card (opus-playbook §6:
 * "無事不報").
 */
export function SinceLastVisit({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  // Captured once, before the mount-effect below overwrites the stamp for
  // next time — this is genuinely "since the PREVIOUS visit", not "since
  // now".
  const [anchorMs] = useState<number | null>(() => readLastVisit());
  const [summaries, setSummaries] = useState<SinceVisitSummary[] | null>(null);

  const nowMs = Date.now();
  const visible = enabled && shouldShowSinceLastVisit(anchorMs, nowMs);

  useEffect(() => {
    if (!visible || anchorMs == null) return;
    let alive = true;
    api.activity
      .list({ limit: FETCH_LIMIT })
      .then((r) => {
        if (alive) setSummaries(summarizeSinceLastVisit(r.events, anchorMs, Date.now()));
      })
      .catch(() => {
        if (alive) setSummaries([]);
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, anchorMs]);

  // Stamp "now" as the anchor for the NEXT visit. Once per mount, independent
  // of whether this visit ends up rendering anything — the clock resets on
  // every visit regardless.
  useEffect(() => {
    if (enabled) writeLastVisit(Date.now());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled]);

  if (!visible || summaries == null || summaries.length === 0) return null;

  const phrases = summaries
    .filter((s) => CATEGORY_MESSAGE_ID[s.type])
    .map((s) => intl.formatMessage({ id: CATEGORY_MESSAGE_ID[s.type] as string }, { count: s.count }));
  if (phrases.length === 0) return null;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{intl.formatMessage({ id: 'home.sinceVisit.title' })}</CardTitle>
      </CardHeader>
      <p className="px-4 pb-4 text-sm text-muted-foreground">{intl.formatList(phrases, { type: 'conjunction' })}</p>
    </Card>
  );
}
