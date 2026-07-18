import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type CostSummary } from '@/lib/api';
import { formatMillicents, formatTokens } from '@/lib/format';

/**
 * UsageSummary — the "用量摘要" KPI mini-row on Home (WP1.5, spec §5.5). A fixed
 * four-tile report strip over the existing `cost.summary` telemetry (24h window):
 * cost / tokens / runs / cache efficiency. Not part of the reorderable widget
 * layout (the widget catalog is gateway-owned) — a fixed report header.
 *
 * Best-effort and honest: if telemetry is uninitialised or the viewer lacks the
 * scope, tiles show "—" rather than a fabricated zero.
 */
export function UsageSummary({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  const [summary, setSummary] = useState<CostSummary | null>(null);

  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    api.cost
      .summary(24)
      .then((r) => alive && setSummary(r))
      .catch(() => alive && setSummary({ available: false }));
    return () => {
      alive = false;
    };
  }, [enabled]);

  const ok = summary?.available === true;
  const tokens = ok
    ? (summary?.total_input_tokens ?? 0) +
      (summary?.total_output_tokens ?? 0) +
      (summary?.total_cache_read_tokens ?? 0) +
      (summary?.total_cache_creation_tokens ?? 0)
    : null;
  const cacheEff =
    ok && typeof summary?.avg_cache_efficiency === 'number'
      ? `${Math.round(summary.avg_cache_efficiency * 100)}%`
      : null;

  const tiles: Array<{ label: string; value: string }> = [
    {
      label: intl.formatMessage({ id: 'home.kpi.cost' }),
      value: ok ? formatMillicents(summary?.total_cost_millicents ?? 0) : '—',
    },
    {
      label: intl.formatMessage({ id: 'home.kpi.tokens' }),
      value: tokens === null ? '—' : formatTokens(tokens),
    },
    {
      label: intl.formatMessage({ id: 'home.kpi.runs' }),
      value: ok ? String(summary?.total_requests ?? 0) : '—',
    },
    {
      label: intl.formatMessage({ id: 'home.kpi.cacheEff' }),
      value: cacheEff ?? '—',
    },
  ];

  return (
    <div className="grid grid-cols-2 gap-px overflow-hidden rounded-xl border border-surface-border bg-surface-border shadow-[var(--surface-shadow)] lg:grid-cols-4">
      {tiles.map((t) => (
        <div key={t.label} className="bg-surface p-4">
          <p className="text-xs text-muted-foreground">{t.label}</p>
          <p className="mt-1 font-mono text-xl font-medium tabular-nums text-foreground">
            {t.value}
          </p>
        </div>
      ))}
    </div>
  );
}
