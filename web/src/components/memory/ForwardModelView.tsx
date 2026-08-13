import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Radar } from 'lucide-react';

import { api, type ForwardAgentSummary, type ForwardPredictionRow } from '@/lib/api';
import { timeAgo } from '@/lib/format';
import { toast } from '@/lib/toast';
import {
  ActorAvatar,
  Card,
  CardContent,
  CollectionPageState,
  useErrorMessage,
} from '@/components/mds';

/**
 * 預測校準 view (Memory page tab) — the first dashboard surface for the
 * v1.53 task forward model + v1.54 calibration layer.
 *
 * Generic by design: LLM→LWM is a platform feature, not an experiment
 * artifact, so this reads only the platform store (`forward.summary` /
 * `forward.recent` over `prediction.db`) and renders for ANY agent — the
 * trading experiment is just one producer among many.
 *
 * Plain-language labels: category / fidelity / source enums are translated
 * to user words (如預期 / 嚴重偏差…), never raw engine terms.
 */

/** Error-category display order — best to worst. */
const CATEGORY_ORDER = ['negligible', 'moderate', 'significant', 'critical'] as const;

function categoryTone(category: string | null): string {
  switch (category) {
    case 'negligible':
      return 'text-emerald-600 dark:text-emerald-400';
    case 'moderate':
      return 'text-foreground';
    case 'significant':
      return 'text-amber-600 dark:text-amber-400';
    case 'critical':
      return 'text-rose-600 dark:text-rose-400';
    default:
      return 'text-muted-foreground';
  }
}

/** Segment fills — semantic ramp (§ Design Principles: emerald = all is
 *  well and should dominate a healthy bar; amber warns; rose errors). */
const CATEGORY_FILL: Record<(typeof CATEGORY_ORDER)[number], string> = {
  negligible: 'bg-emerald-500/80',
  moderate: 'bg-chart-3',
  significant: 'bg-amber-500/80',
  critical: 'bg-rose-500/80',
};

/**
 * One stacked distribution bar over the four error categories + a compact
 * legend — the house inline-bar idiom (see `TelemetryCard`), condensed to a
 * single glanceable strip instead of four text rows.
 */
function CategoryDistribution({
  categories,
  settled,
}: {
  categories: Record<string, number>;
  settled: number;
}) {
  const intl = useIntl();
  const parts = CATEGORY_ORDER.map((c) => ({ c, n: categories[c] ?? 0 })).filter((p) => p.n > 0);
  if (settled === 0 || parts.length === 0) return null;
  return (
    <div className="space-y-1.5 border-t border-surface-border pt-3">
      <div
        className="flex h-2 w-full overflow-hidden rounded-full bg-muted"
        role="img"
        aria-label={parts
          .map((p) => `${intl.formatMessage({ id: `forward.category.${p.c}` })} ${p.n}`)
          .join(', ')}
      >
        {parts.map((p) => (
          <div
            key={p.c}
            className={CATEGORY_FILL[p.c]}
            style={{ width: `${(p.n / settled) * 100}%` }}
          />
        ))}
      </div>
      <div className="flex flex-wrap gap-x-3 gap-y-0.5">
        {parts.map((p) => (
          <span key={p.c} className="flex items-center gap-1 text-xs">
            <span className={`size-1.5 rounded-full ${CATEGORY_FILL[p.c]}`} />
            <span className={categoryTone(p.c)}>
              {intl.formatMessage({ id: `forward.category.${p.c}` })}
            </span>
            <span className="font-mono tabular-nums text-muted-foreground">{p.n}</span>
          </span>
        ))}
      </div>
    </div>
  );
}

export function ForwardModelView({ selectedAgent }: { selectedAgent: string }) {
  const intl = useIntl();
  const errorText = useErrorMessage();
  const [summaries, setSummaries] = useState<ForwardAgentSummary[]>([]);
  const [rows, setRows] = useState<ForwardPredictionRow[]>([]);
  const [windowScanned, setWindowScanned] = useState(0);
  const [windowCap, setWindowCap] = useState(0);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    let notified = false;
    const onFailure = (e: unknown) => {
      console.warn('[api]', e);
      if (!notified) {
        notified = true;
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: errorText(e) }));
      }
      return null;
    };
    Promise.all([
      api.forward.summary(selectedAgent || undefined).catch(onFailure),
      api.forward.recent(selectedAgent || undefined, 50).catch(onFailure),
    ])
      .then(([summary, recent]) => {
        if (!alive) return;
        setSummaries(summary?.agents ?? []);
        setWindowScanned(summary?.window_scanned ?? 0);
        setWindowCap(summary?.window_cap ?? 0);
        setRows(recent?.predictions ?? []);
      })
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
    // errorText is stable from the hook; intl from context.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedAgent]);

  if (loading) return <CollectionPageState state="loading" />;

  if (summaries.length === 0) {
    return (
      <CollectionPageState
        state="empty"
        icon={Radar}
        title={intl.formatMessage({ id: 'forward.empty.title' })}
        description={intl.formatMessage({ id: 'forward.empty.desc' })}
      />
    );
  }

  return (
    <div className="space-y-4">
      {/* Per-agent calibration summary cards. */}
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
        {summaries.map((s) => (
          <Card key={s.agent_id} data-size="sm">
            <CardContent className="space-y-3">
              <div className="flex items-center gap-2">
                <ActorAvatar actorType="agent" size="sm" name={s.agent_id} />
                <h3 className="truncate text-sm font-medium text-foreground">{s.agent_id}</h3>
                {s.last_settled_at && (
                  <span className="ml-auto font-mono text-xs tabular-nums text-muted-foreground">
                    {timeAgo(s.last_settled_at)}
                  </span>
                )}
              </div>

              <div className="grid grid-cols-3 gap-2">
                <div className="text-center">
                  <p className="font-mono text-lg font-medium tabular-nums text-foreground">{s.total}</p>
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'forward.metric.total' })}
                  </p>
                </div>
                <div className="text-center">
                  <p className="font-mono text-lg font-medium tabular-nums text-foreground">{s.settled}</p>
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'forward.metric.settled' })}
                  </p>
                </div>
                <div className="text-center">
                  <p className="font-mono text-lg font-medium tabular-nums text-foreground">
                    {s.avg_brier != null ? s.avg_brier.toFixed(2) : '—'}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {intl.formatMessage({ id: 'forward.metric.brier' })}
                  </p>
                </div>
              </div>

              {/* Error-category distribution — one glanceable stacked bar. */}
              <CategoryDistribution categories={s.categories} settled={s.settled} />
            </CardContent>
          </Card>
        ))}
      </div>

      {/* Recent predictions — newest first, settled rows show their grade. */}
      {rows.length > 0 && (
        <section className="space-y-2">
          <h2 className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'forward.recent.title' })}
          </h2>
          <Card data-size="sm">
            <CardContent className="divide-y divide-surface-border">
              {rows.map((r) => (
                <div key={r.prediction_id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2 text-xs">
                  <span className="font-mono tabular-nums text-muted-foreground">
                    {timeAgo(r.settled_at ?? r.created_at)}
                  </span>
                  <span className="truncate text-foreground" title={r.task_id}>
                    {r.task_id.slice(0, 8)} · R{r.round}
                  </span>
                  <span className="text-muted-foreground">
                    {intl.formatMessage({ id: `forward.source.${r.source}`, defaultMessage: r.source })}
                  </span>
                  <span className={`ml-auto ${categoryTone(r.category)}`}>
                    {r.settled_at == null
                      ? intl.formatMessage({ id: 'forward.pending' })
                      : r.category
                        ? intl.formatMessage({ id: `forward.category.${r.category}`, defaultMessage: r.category })
                        : '—'}
                  </span>
                  {r.brier != null && (
                    <span className="font-mono tabular-nums text-muted-foreground">{r.brier.toFixed(2)}</span>
                  )}
                </div>
              ))}
            </CardContent>
          </Card>
        </section>
      )}

      {/* Honest window note: the summary covers a bounded newest window. */}
      {windowCap > 0 && windowScanned >= windowCap && (
        <p className="text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'forward.window.note' }, { cap: windowCap })}
        </p>
      )}
    </div>
  );
}
