import { useCallback, useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Layers, CheckCircle2, XCircle, HelpCircle, AlertTriangle } from 'lucide-react';
import { api, type PresetSummary, type PresetResolution } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import {
  CollectionPageHeader,
  CollectionPageState,
  ErrorState,
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  Badge,
  ActorAvatar,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  type BadgeProps,
} from '@/components/mds';

/**
 * PresetsPage (`/manage`-adjacent `/presets`, agent preset P1 dashboard card,
 * WP-7I 2026-08-16) — 職務組合.
 *
 * The preset design (`commercial/docs/DESIGN-agent-presets-2026-08.md`) scopes
 * its first shippable slice (§9 P1) to "reference + layer + leave a trail",
 * explicitly EXCLUDING any switching UI — binding an agent to a preset stays a
 * CLI-only operation (`duduclaw preset bind`) in this wave. This page is the
 * P1 item ⑦ read-only dashboard card the design called out as still missing:
 * the catalog of available presets (`presets.list`) plus, for every AI staff
 * member, its live binding and which of its own `agent.toml` fields override
 * the preset (`presets.status`, §1 R1.4 "已覆寫"). Both RPCs are read-only —
 * this page issues no write of any kind.
 *
 * A preset that fails to parse still appears in the catalog (carrying
 * `.error`) rather than being silently dropped, mirroring the CLI's own
 * `duduclaw preset list`. An agent whose preset failed to resolve shows
 * `unresolved` + the reason — per design §1 R1.5, that agent still runs (on
 * its own `agent.toml`, fail-closed to no preset), it just isn't wearing the
 * preset it asked for.
 */
export function PresetsPage() {
  const intl = useIntl();
  const t = useCallback(
    (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values),
    [intl],
  );
  const connState = useConnectionStore((s) => s.state);
  const agents = useAgentsStore((s) => s.agents);
  const fetchAgents = useAgentsStore((s) => s.fetchAgents);

  const [presets, setPresets] = useState<PresetSummary[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  // agent name → resolution, or `null` while its own `presets.status` call is
  // still in flight / failed. Absent entries simply haven't resolved yet.
  const [statuses, setStatuses] = useState<Record<string, PresetResolution | null>>({});

  const load = useCallback(async () => {
    setError(null);
    try {
      const [presetRes] = await Promise.all([api.presets.list(), fetchAgents()]);
      setPresets(presetRes?.presets ?? []);
    } catch (e) {
      setError(e);
    }
  }, [fetchAgents]);

  useEffect(() => {
    if (connState === 'authenticated') void load();
  }, [connState, load]);

  // Once the roster is known, fetch each agent's own binding status in
  // parallel. A single agent's failed lookup never blocks the rest — the
  // table shows that one row as "couldn't load" rather than losing every row.
  useEffect(() => {
    if (connState !== 'authenticated' || agents.length === 0) return;
    let cancelled = false;
    void Promise.all(
      agents.map((a) =>
        api.presets
          .status(a.name)
          .then((res) => [a.name, res?.resolution ?? null] as const)
          .catch(() => [a.name, null] as const),
      ),
    ).then((pairs) => {
      if (cancelled) return;
      setStatuses((prev) => {
        const next = { ...prev };
        for (const [name, resolution] of pairs) next[name] = resolution;
        return next;
      });
    });
    return () => {
      cancelled = true;
    };
  }, [connState, agents]);

  const agentLabel = useMemo(
    () => new Map(agents.map((a) => [a.name, a.display_name || a.name])),
    [agents],
  );

  return (
    <div className="flex h-full flex-col">
      <CollectionPageHeader
        icon={Layers}
        title={t('presets.title')}
        count={presets?.length}
        description={t('presets.subtitle')}
      />

      <div className="flex-1 space-y-5 overflow-y-auto p-5">
        {presets === null && error != null ? (
          <ErrorState icon={Layers} error={error} onRetry={() => void load()} />
        ) : presets === null ? (
          <CollectionPageState state="loading" title={t('presets.title')} />
        ) : (
          <>
            <Card>
              <CardHeader>
                <CardTitle>{t('presets.section.catalog')}</CardTitle>
                <CardDescription>{t('presets.subtitle')}</CardDescription>
              </CardHeader>
              <CardContent>
                {presets.length === 0 ? (
                  <CollectionPageState
                    state="empty"
                    icon={Layers}
                    title={t('presets.empty.catalog')}
                    description={t('presets.empty.catalog.hint')}
                  />
                ) : (
                  <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                    {presets.map((p) => (
                      <PresetCatalogCard key={p.id} preset={p} t={t} />
                    ))}
                  </div>
                )}
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>{t('presets.section.bindings')}</CardTitle>
              </CardHeader>
              <CardContent>
                {agents.length === 0 ? (
                  <CollectionPageState state="empty" icon={Layers} title={t('presets.empty.bindings')} />
                ) : (
                  <div className="overflow-x-auto">
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>{t('presets.col.agent')}</TableHead>
                          <TableHead>{t('presets.col.status')}</TableHead>
                          <TableHead>{t('presets.col.preset')}</TableHead>
                          <TableHead>{t('presets.col.overridden')}</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {agents.map((a) => (
                          <AgentBindingRow
                            key={a.name}
                            agentId={a.name}
                            displayName={agentLabel.get(a.name) ?? a.name}
                            resolution={statuses[a.name]}
                            t={t}
                          />
                        ))}
                      </TableBody>
                    </Table>
                  </div>
                )}
              </CardContent>
            </Card>
          </>
        )}
      </div>
    </div>
  );
}

function PresetCatalogCard({
  preset,
  t,
}: {
  preset: PresetSummary;
  t: (id: string, values?: Record<string, string | number>) => string;
}) {
  return (
    <div className="rounded-lg border border-surface-border p-3">
      <div className="flex items-start justify-between gap-2">
        <span className="truncate font-mono text-sm font-medium text-foreground">{preset.id}</span>
        {preset.version && (
          <Badge variant="outline" className="shrink-0 font-mono text-[11px]">
            {t('presets.catalog.version', { version: preset.version })}
          </Badge>
        )}
      </div>
      {preset.label && <p className="mt-1 truncate text-sm text-foreground">{preset.label}</p>}
      {preset.error ? (
        <p className="mt-2 flex items-start gap-1.5 text-xs text-destructive">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
          <span>{preset.error}</span>
        </p>
      ) : (
        <p className="mt-2 line-clamp-2 text-xs text-muted-foreground">
          {preset.description || t('presets.catalog.noDescription')}
        </p>
      )}
    </div>
  );
}

/** Status badge tone per `PresetResolution['state']` (plus the client-only
 *  "couldn't load" state for a failed `presets.status` call). */
function resolutionTone(
  state: PresetResolution['state'] | 'error',
): { variant: NonNullable<BadgeProps['variant']>; icon: typeof CheckCircle2 } {
  switch (state) {
    case 'applied':
      return { variant: 'secondary', icon: CheckCircle2 };
    case 'unresolved':
      return { variant: 'destructive', icon: XCircle };
    case 'error':
      return { variant: 'destructive', icon: AlertTriangle };
    default:
      return { variant: 'outline', icon: HelpCircle };
  }
}

function AgentBindingRow({
  agentId,
  displayName,
  resolution,
  t,
}: {
  agentId: string;
  displayName: string;
  resolution: PresetResolution | null | undefined;
  t: (id: string, values?: Record<string, string | number>) => string;
}) {
  // `undefined` = still loading; `null` = the per-agent status call failed.
  const state = resolution === undefined ? undefined : resolution === null ? 'error' : resolution.state;
  const tone = state ? resolutionTone(state) : null;
  const Icon = tone?.icon;

  return (
    <TableRow>
      <TableCell>
        <span className="flex items-center gap-1.5" title={agentId}>
          <ActorAvatar actorType="agent" size="xs" name={displayName} />
          <span className="truncate text-sm">{displayName}</span>
        </span>
      </TableCell>
      <TableCell>
        {state === undefined ? (
          <span className="text-xs text-muted-foreground">…</span>
        ) : (
          <Badge variant={tone?.variant} className="gap-1">
            {Icon && <Icon className="size-3" />}
            {t(`presets.badge.${state}`)}
          </Badge>
        )}
      </TableCell>
      <TableCell>
        {resolution?.state === 'applied' || resolution?.state === 'unresolved' ? (
          <div className="flex flex-col gap-0.5">
            <span className="font-mono text-xs text-foreground">
              {resolution.preset_id}
              {resolution.version ? `@${resolution.version}` : ''}
            </span>
            {resolution.state === 'applied' && resolution.label && (
              <span className="text-xs text-muted-foreground">{resolution.label}</span>
            )}
            {resolution.state === 'unresolved' && resolution.reason && (
              <span className="text-xs text-destructive">
                {t('presets.reason', { reason: resolution.reason })}
              </span>
            )}
          </div>
        ) : (
          <span className="text-xs text-muted-foreground">—</span>
        )}
      </TableCell>
      <TableCell>
        {resolution?.state === 'applied' && resolution.changed_fields && resolution.changed_fields.length > 0 ? (
          <div className="flex flex-wrap gap-1">
            {resolution.changed_fields.map((f) => (
              <Badge key={f} variant="outline" className="font-mono text-[11px]">
                {f}
              </Badge>
            ))}
          </div>
        ) : (
          <span className="text-xs text-muted-foreground">{t('presets.overridden.none')}</span>
        )}
      </TableCell>
    </TableRow>
  );
}

export default PresetsPage;
