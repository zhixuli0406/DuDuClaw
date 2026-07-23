import { useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import {
  FolderSearch,
  Eye,
  Footprints,
  BellRing,
  Workflow,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import type { OsAgentStatus, OsSettingsUpdateParams } from '@/lib/api';
import {
  ActorAvatar,
  Badge,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Input,
  Switch,
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/mds';

/**
 * OsAgentCard — one AI employee's OS-native settings + live stats (P4-3
 * overview section). Every switch fires `os.settings.update` immediately
 * (AutopilotTab convention); every number field commits on blur/Enter so
 * typing a value doesn't fire an RPC per keystroke. The parent owns the
 * request lifecycle (loading/toast/refetch) — this component only reports
 * intent via `onUpdate` and reads `savingField` to disable the row in flight.
 */

type UpdatePatch = Omit<OsSettingsUpdateParams, 'agent_id'>;

export interface OsAgentCardProps {
  agent: OsAgentStatus;
  displayName: string;
  /** True when turning `os_native` on for THIS agent would exceed the
   *  edition's OS-native seat quota (already-on agents are never blocked). */
  quotaBlocked: string | null;
  /** `${agent_id}:${fieldKey}` of the field currently in flight, if any. */
  savingField: string | null;
  onUpdate: (patch: UpdatePatch, fieldKey: string) => void;
}

/** Numeric field that only commits on blur / Enter (never per-keystroke) —
 *  avoids firing an `os.settings.update` RPC on every digit typed. */
function CommitNumberField({
  value,
  min,
  max,
  step,
  disabled,
  ariaLabel,
  onCommit,
}: {
  value: number;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  ariaLabel?: string;
  onCommit: (v: number) => void;
}) {
  const [local, setLocal] = useState(String(value));
  useEffect(() => {
    setLocal(String(value));
  }, [value]);

  const commit = () => {
    const n = Number(local);
    if (!Number.isFinite(n)) {
      setLocal(String(value));
      return;
    }
    const lo = min ?? -Infinity;
    const hi = max ?? Infinity;
    const clamped = Math.min(hi, Math.max(lo, Math.round(n)));
    setLocal(String(clamped));
    if (clamped !== value) onCommit(clamped);
  };

  return (
    <Input
      type="number"
      value={local}
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      aria-label={ariaLabel}
      className="w-24"
      onChange={(e) => setLocal(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
      }}
    />
  );
}

/** A row inside the divide-y card body: label (+ optional icon/badge) on the
 *  left, control or read-only value on the right. Mirrors `SettingsRow`'s
 *  visual rhythm without pulling in the full Settings-shell contract. */
function AgentRow({
  icon: Icon,
  label,
  description,
  children,
}: {
  icon?: React.ComponentType<{ className?: string }>;
  label: ReactNode;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex min-h-14 flex-col gap-2 px-4 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-4">
      <div className="min-w-0 space-y-0.5">
        <div className="flex items-center gap-1.5 text-sm font-medium">
          {Icon && <Icon className="size-3.5 shrink-0 text-muted-foreground" />}
          {label}
        </div>
        {description && <div className="text-xs text-muted-foreground">{description}</div>}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

export function OsAgentCard({ agent, displayName, quotaBlocked, savingField, onUpdate }: OsAgentCardProps) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const savingKey = (field: string) => `${agent.agent_id}:${field}`;
  const isSaving = (field: string) => savingField === savingKey(field);

  const osNativeControl = (
    <Switch
      checked={agent.os_native}
      disabled={Boolean(quotaBlocked) || isSaving('os_native')}
      aria-label={t('os.agent.osNative')}
      onCheckedChange={(v) => onUpdate({ os_native: Boolean(v) }, 'os_native')}
    />
  );

  return (
    <Card className="overflow-hidden py-0">
      <CardHeader className="flex flex-row items-center justify-between gap-3 space-y-0 border-b border-surface-border px-4 py-3.5">
        <div className="flex min-w-0 items-center gap-2.5">
          <ActorAvatar actorType="agent" name={displayName} size="md" />
          <div className="min-w-0">
            <CardTitle className="truncate text-sm font-medium leading-snug">{displayName}</CardTitle>
            {agent.induced_rules_count > 0 && (
              <p className="mt-0.5 flex items-center gap-1 text-xs text-muted-foreground">
                <Workflow className="size-3" />
                {t('os.agent.inducedRules', { count: agent.induced_rules_count })}
              </p>
            )}
          </div>
        </div>
        {quotaBlocked ? (
          <Tooltip>
            <TooltipTrigger
              render={
                <span className="inline-flex cursor-not-allowed" tabIndex={0}>
                  {osNativeControl}
                </span>
              }
            />
            <TooltipContent>{quotaBlocked}</TooltipContent>
          </Tooltip>
        ) : (
          osNativeControl
        )}
      </CardHeader>
      <CardContent className="p-0">
        {!agent.os_native ? (
          <p className="px-4 py-6 text-center text-sm text-muted-foreground">
            {t('os.agent.disabled.hint')}
          </p>
        ) : (
          <div className="divide-y divide-surface-border">
            {/* Watch paths + live stats (read-only — edited on the agent's own edit page). */}
            <div className="space-y-2 px-4 py-3">
              <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
                <span className="flex items-center gap-1.5 text-sm font-medium">
                  <FolderSearch className="size-3.5 shrink-0 text-muted-foreground" />
                  {t('os.agent.watch.title')}
                </span>
                <span className="font-mono text-xs tabular-nums text-muted-foreground">
                  {t('os.agent.watch.stats', { events: agent.watch.events, dropped: agent.watch.dropped })}
                </span>
              </div>
              {agent.watch.paths.length === 0 ? (
                <p className="text-xs text-muted-foreground">{t('os.agent.watch.empty')}</p>
              ) : (
                <div className="flex flex-wrap gap-1.5">
                  {agent.watch.paths.map((p) => (
                    <Badge key={p} variant="outline" title={p} className="max-w-full truncate font-mono text-[11px] font-normal">
                      {p}
                    </Badge>
                  ))}
                </div>
              )}
            </div>

            {/* Frontmost polling. */}
            <AgentRow
              icon={Eye}
              label={
                <span className="flex items-center gap-1.5">
                  {t('os.agent.frontmost')}
                  <Badge
                    variant="secondary"
                    className={cn(
                      'font-normal',
                      agent.frontmost.running ? 'bg-success/15 text-success' : 'bg-muted text-muted-foreground',
                    )}
                  >
                    {agent.frontmost.running ? t('os.agent.frontmost.running') : t('os.agent.frontmost.idle')}
                  </Badge>
                </span>
              }
              description={t('os.agent.frontmost.desc')}
            >
              <CommitNumberField
                value={agent.frontmost.poll_secs}
                min={0}
                max={3600}
                step={5}
                disabled={isSaving('frontmost_poll_secs')}
                ariaLabel={t('os.agent.frontmost')}
                onCommit={(v) => onUpdate({ frontmost_poll_secs: v }, 'frontmost_poll_secs')}
              />
            </AgentRow>

            {/* Digital-footprint memory distillation. */}
            <AgentRow icon={Footprints} label={t('os.agent.footprint')} description={t('os.agent.footprint.desc')}>
              <Switch
                checked={agent.footprint}
                disabled={isSaving('footprint')}
                aria-label={t('os.agent.footprint')}
                onCheckedChange={(v) => onUpdate({ footprint: Boolean(v) }, 'footprint')}
              />
            </AgentRow>

            {/* Proactive outreach. */}
            <AgentRow icon={BellRing} label={t('os.agent.proactive.enabled')} description={t('os.agent.proactive.enabled.desc')}>
              <Switch
                checked={agent.proactive.enabled}
                disabled={isSaving('proactive.enabled')}
                aria-label={t('os.agent.proactive.enabled')}
                onCheckedChange={(v) =>
                  onUpdate({ proactive: { enabled: Boolean(v) } }, 'proactive.enabled')
                }
              />
            </AgentRow>
            <AgentRow label={t('os.agent.proactive.threshold')} description={t('os.agent.proactive.threshold.desc')}>
              <CommitNumberField
                value={agent.proactive.base_threshold}
                min={1}
                max={5}
                step={1}
                disabled={isSaving('proactive.base_threshold')}
                ariaLabel={t('os.agent.proactive.threshold')}
                onCommit={(v) => onUpdate({ proactive: { base_threshold: v } }, 'proactive.base_threshold')}
              />
            </AgentRow>
            <AgentRow label={t('os.agent.proactive.maxPerHour')} description={t('os.agent.proactive.maxPerHour.desc')}>
              <CommitNumberField
                value={agent.proactive.max_per_hour}
                min={0}
                max={1000}
                step={1}
                disabled={isSaving('proactive.max_per_hour')}
                ariaLabel={t('os.agent.proactive.maxPerHour')}
                onCommit={(v) => onUpdate({ proactive: { max_per_hour: v } }, 'proactive.max_per_hour')}
              />
            </AgentRow>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
