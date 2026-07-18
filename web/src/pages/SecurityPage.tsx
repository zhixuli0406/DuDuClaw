import { useEffect, useState, useCallback, useRef, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { api, type AuditEvent, type KillswitchConfig } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { AdvancedSection, ConfirmDialog } from '@/components/settings/controls';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  Badge,
  Button,
  Empty,
  Input,
  Switch,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
  ActorAvatar,
  Skeleton,
  SettingsSaveState,
  type SettingsSaveStatus,
} from '@/components/mds';
import { cn } from '@/lib/utils';
import {
  Shield,
  Lock,
  ShieldCheck,
  Users,
  AlertTriangle,
  FileWarning,
  History,
  OctagonX,
} from 'lucide-react';

interface SecurityStatus {
  credential_proxy: { active: boolean; vault_backend: string; injected_secrets: number };
  mount_guard: { rules: Array<{ path: string; access: string }> };
  rbac: Array<{
    agent_id: string; role: string;
    tool_use: boolean; web_access: boolean;
    file_write: boolean; shell_exec: boolean; delegate: boolean;
  }>;
  rate_limiter: { requests_per_minute: number; concurrent_requests: number };
  soul_drift: Array<{ agent_id: string; soul_exists: boolean; gvu_enabled: boolean }>;
}

export function SecurityPage() {
  const intl = useIntl();
  const [auditEvents, setAuditEvents] = useState<AuditEvent[]>([]);
  const [auditLoading, setAuditLoading] = useState(false);
  const [status, setStatus] = useState<SecurityStatus | null>(null);
  const connectionState = useConnectionStore((s) => s.state);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    setAuditLoading(true);
    api.security
      .auditLog(30)
      .then((res) => setAuditEvents(res?.events ?? []))
      .catch(() => setAuditEvents([]))
      .finally(() => setAuditLoading(false));

    api.security
      .status()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, [connectionState]);

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      {/* Slim header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Shield className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'nav.security' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'app.subtitle' })}</p>
          </div>
        </div>
      </div>

      {/* KPI overview — scalar security metrics (§5.5 divide group) */}
      <div className="grid gap-px overflow-hidden rounded-xl border border-surface-border bg-surface-border sm:grid-cols-2 lg:grid-cols-4">
        <KpiCell
          label={intl.formatMessage({ id: 'security.injectedSecrets' })}
          value={status?.credential_proxy?.injected_secrets ?? 0}
        />
        <KpiCell
          label={intl.formatMessage({ id: 'security.reqPerMin' })}
          value={status?.rate_limiter?.requests_per_minute ?? 60}
        />
        <KpiCell
          label={intl.formatMessage({ id: 'security.concurrent' })}
          value={status?.rate_limiter?.concurrent_requests ?? 5}
        />
        <KpiCell
          label={intl.formatMessage({ id: 'security.mountGuard.title' })}
          value={status?.mount_guard?.rules?.length ?? 0}
        />
      </div>

      <div className="grid gap-6 lg:grid-cols-2">
        {/* Audit Log */}
        <SecurityCard
          icon={History}
          title={intl.formatMessage({ id: 'security.audit.title' })}
          description={intl.formatMessage({ id: 'security.audit.desc' })}
        >
          {auditLoading ? (
            <div className="space-y-2">
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-9 w-full" />
            </div>
          ) : auditEvents.length === 0 ? (
            <Empty icon={History} title={intl.formatMessage({ id: 'security.audit.empty' })} />
          ) : (
            <div className="max-h-64 space-y-2 overflow-y-auto">
              {auditEvents.map((evt, i) => (
                <AuditRow key={`${evt.timestamp}-${i}`} event={evt} />
              ))}
            </div>
          )}
        </SecurityCard>

        {/* Credential Proxy */}
        <SecurityCard
          icon={Lock}
          title={intl.formatMessage({ id: 'security.credentialProxy.title' })}
          description={intl.formatMessage({ id: 'security.credentialProxy.desc' })}
        >
          <div className="space-y-3">
            <StatusRow
              label={intl.formatMessage({ id: 'security.proxyStatus' })}
              status={status?.credential_proxy?.active ? 'active' : 'inactive'}
            />
            <StatusRow
              label={intl.formatMessage({ id: 'security.vaultBackend' })}
              value={status?.credential_proxy?.vault_backend ?? '—'}
            />
          </div>
        </SecurityCard>

        {/* Mount Guard */}
        <SecurityCard
          icon={ShieldCheck}
          title={intl.formatMessage({ id: 'security.mountGuard.title' })}
          description={intl.formatMessage({ id: 'security.mountGuard.desc' })}
        >
          <div className="space-y-2">
            {status?.mount_guard?.rules && status.mount_guard.rules.length > 0 ? (
              status.mount_guard.rules.map((rule) => (
                <RuleRow key={rule.path} path={rule.path} access={rule.access} />
              ))
            ) : (
              <p className="py-2 text-center text-sm text-muted-foreground">
                {intl.formatMessage({ id: 'common.noData' })}
              </p>
            )}
          </div>
        </SecurityCard>

        {/* RBAC */}
        <SecurityCard
          icon={Users}
          title={intl.formatMessage({ id: 'security.rbac.title' })}
          description={intl.formatMessage({ id: 'security.rbac.desc' })}
          className="lg:col-span-2"
        >
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>{intl.formatMessage({ id: 'security.rbac.agent' })}</TableHead>
                <TableHead className="text-center">{intl.formatMessage({ id: 'security.rbac.tool' })}</TableHead>
                <TableHead className="text-center">{intl.formatMessage({ id: 'security.rbac.web' })}</TableHead>
                <TableHead className="text-center">{intl.formatMessage({ id: 'security.rbac.file' })}</TableHead>
                <TableHead className="text-center">{intl.formatMessage({ id: 'security.rbac.shell' })}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {(status?.rbac ?? []).map((agent) => (
                <TableRow key={agent.agent_id}>
                  <TableCell>
                    <span className="flex items-center gap-2">
                      <ActorAvatar actorType="agent" name={agent.agent_id} size="sm" />
                      <span className="font-mono text-xs text-foreground">{agent.agent_id}</span>
                      <span className="text-xs text-muted-foreground">({agent.role})</span>
                    </span>
                  </TableCell>
                  <PermCell allowed={agent.tool_use} />
                  <PermCell allowed={agent.web_access} />
                  <PermCell allowed={agent.file_write} />
                  <PermCell allowed={agent.shell_exec} />
                </TableRow>
              ))}
              {(!status?.rbac || status.rbac.length === 0) && (
                <TableRow>
                  <TableCell colSpan={5} className="py-4 text-center text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'common.noData' })}
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </SecurityCard>
      </div>

      {/* Killswitch (KS) — editable, dangerous */}
      <KillswitchSection />
    </div>
  );
}

// ── Killswitch editor (KS) ──────────────────────────────────────

function KillswitchSection() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [config, setConfig] = useState<KillswitchConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    try {
      setConfig(await api.killswitch.get());
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => {
    if (connectionState !== 'authenticated') return;
    load();
  }, [connectionState, load]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    try {
      await api.killswitch.update({
        triggers: config.triggers,
        circuit_breaker: config.circuit_breaker,
        safety_words: config.safety_words,
        defensive_prompt: config.defensive_prompt,
      });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const saveStatus: SettingsSaveStatus = saving ? 'saving' : saved ? 'saved' : 'idle';

  return (
    <>
    <SecurityCard
      icon={OctagonX}
      title={intl.formatMessage({ id: 'killswitch.title' })}
      description={intl.formatMessage({ id: 'killswitch.desc' })}
    >
      {!config ? (
        <p className="py-4 text-center text-sm text-muted-foreground">{intl.formatMessage({ id: 'common.loading' })}</p>
      ) : (
        <div className="space-y-6">
          {/* Triggers */}
          <div className="space-y-3">
            <h3 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'killswitch.triggers' })}</h3>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label={intl.formatMessage({ id: 'killswitch.maxRepliesPerMinute' })} hint={intl.formatMessage({ id: 'killswitch.maxRepliesPerMinute.help' })}>
                <Input type="number" min={1} max={10000} value={config.triggers.max_replies_per_minute} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, max_replies_per_minute: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.maxConsecutiveErrors' })} hint={intl.formatMessage({ id: 'killswitch.maxConsecutiveErrors.help' })}>
                <Input type="number" min={1} max={1000} value={config.triggers.max_consecutive_errors} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, max_consecutive_errors: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.errorRateThreshold' })} hint={intl.formatMessage({ id: 'killswitch.errorRateThreshold.help' })}>
                <Input type="number" min={0} max={1} step={0.01} value={config.triggers.error_rate_threshold} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, error_rate_threshold: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.costLimitUsd' })} hint={intl.formatMessage({ id: 'killswitch.costLimitUsd.help' })}>
                <Input type="number" min={0} step={0.01} value={config.triggers.cost_limit_usd} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, cost_limit_usd: Number(e.target.value) } })} />
              </Field>
            </div>
          </div>

          {/* Circuit breaker — advanced thresholds */}
          <AdvancedSection storageKey="security.killswitch" label={intl.formatMessage({ id: 'killswitch.advanced' })}>
            <h3 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'killswitch.circuitBreaker' })}</h3>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label={intl.formatMessage({ id: 'killswitch.frequencyWindowSecs' })} hint="1-86400">
                <Input type="number" min={1} max={86400} value={config.circuit_breaker.frequency_window_secs} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, frequency_window_secs: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.frequencyMaxReplies' })} hint="1-10000">
                <Input type="number" min={1} max={10000} value={config.circuit_breaker.frequency_max_replies} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, frequency_max_replies: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.similarityThreshold' })} hint="0.0-1.0">
                <Input type="number" min={0} max={1} step={0.01} value={config.circuit_breaker.similarity_threshold} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, similarity_threshold: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.tokenExplosionMultiplier' })} hint="1.0-1000.0">
                <Input type="number" min={1} max={1000} step={0.1} value={config.circuit_breaker.token_explosion_multiplier} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, token_explosion_multiplier: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.cooldownSecs' })} hint="0-86400">
                <Input type="number" min={0} max={86400} value={config.circuit_breaker.cooldown_secs} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, cooldown_secs: Number(e.target.value) } })} />
              </Field>
              <Field label={intl.formatMessage({ id: 'killswitch.halfOpenAllowCount' })} hint="1-1000">
                <Input type="number" min={1} max={1000} value={config.circuit_breaker.half_open_allow_count} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, half_open_allow_count: Number(e.target.value) } })} />
              </Field>
            </div>
          </AdvancedSection>

          {/* Safety words */}
          <div className="space-y-4 border-t border-surface-border pt-4">
            <h3 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'killswitch.safetyWords' })}</h3>
            <div className="space-y-4">
              {(['stop', 'stop_all', 'resume', 'status'] as const).map((key) => (
                <Field key={key} label={intl.formatMessage({ id: `killswitch.safetyWords.${key}` })}>
                  <ChipEditor
                    values={config.safety_words[key]}
                    onChange={(v) => setConfig({ ...config, safety_words: { ...config.safety_words, [key]: v } })}
                    placeholder={key}
                    addLabel={intl.formatMessage({ id: 'common.add' })}
                  />
                </Field>
              ))}
            </div>
          </div>

          {/* Defensive prompt */}
          <div className="space-y-4 border-t border-surface-border pt-4">
            <h3 className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'killswitch.defensivePrompt' })}</h3>
            <div className="flex items-center justify-between py-1">
              <span className="text-sm text-foreground">{intl.formatMessage({ id: 'killswitch.defensivePrompt.enabled' })}</span>
              <Switch
                checked={config.defensive_prompt.enabled}
                onCheckedChange={(checked) => setConfig({ ...config, defensive_prompt: { ...config.defensive_prompt, enabled: checked } })}
              />
            </div>
            <Field label={intl.formatMessage({ id: 'killswitch.defensivePrompt.languages' })}>
              <ChipEditor
                values={config.defensive_prompt.languages}
                onChange={(v) => setConfig({ ...config, defensive_prompt: { ...config.defensive_prompt, languages: v } })}
                placeholder="zh-TW"
                addLabel={intl.formatMessage({ id: 'common.add' })}
              />
            </Field>
          </div>

          <div className="flex items-center justify-end gap-3 pt-2">
            <SettingsSaveState
              status={saveStatus}
              savingLabel={intl.formatMessage({ id: 'common.saving' })}
              savedLabel={intl.formatMessage({ id: 'common.saved' })}
              errorLabel={intl.formatMessage({ id: 'common.saveError' })}
            />
            <Button variant="destructive" onClick={() => setConfirmOpen(true)} disabled={saving}>
              {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
            </Button>
          </div>
        </div>
      )}
    </SecurityCard>

    {/* Killswitch is a dangerous, gateway-wide behavioral control — gate Save
        behind an explicit destructive confirmation. */}
    <ConfirmDialog
      open={confirmOpen}
      onClose={() => setConfirmOpen(false)}
      onConfirm={() => { setConfirmOpen(false); handleSave(); }}
      title={intl.formatMessage({ id: 'killswitch.confirmTitle' })}
      message={intl.formatMessage({
        id: 'killswitch.confirmSave',
        defaultMessage: 'Save kill-switch thresholds now? Most changes only take effect after a gateway restart.',
      })}
      confirmLabel={intl.formatMessage({ id: 'common.save' })}
      busy={saving}
    />
    </>
  );
}

// ── Shared building blocks ──────────────────────────────────────

/** Stacked label + control + hint block used throughout the killswitch editor. */
function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground">{label}</label>
      {children}
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  );
}

function KpiCell({ label, value }: { label: string; value: number }) {
  return (
    <div className="bg-card p-4">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums text-foreground">{value}</p>
    </div>
  );
}

function PermCell({ allowed }: { allowed: boolean }) {
  return (
    <TableCell className="text-center">
      {allowed ? (
        <span className="text-success">&#10003;</span>
      ) : (
        <span className="text-muted-foreground">&#10005;</span>
      )}
    </TableCell>
  );
}

function AuditRow({ event }: { event: AuditEvent }) {
  const severityBadge: Record<string, { variant: 'secondary' | 'destructive'; className?: string }> = {
    info: { variant: 'secondary' },
    warning: { variant: 'secondary', className: 'bg-warning/15 text-warning' },
    critical: { variant: 'destructive' },
  };
  const severityIconColor: Record<string, string> = {
    info: 'text-muted-foreground',
    warning: 'text-warning',
    critical: 'text-destructive',
  };

  const SevIcon = event.severity === 'critical' ? AlertTriangle
    : event.severity === 'warning' ? FileWarning
    : Shield;

  const badge = severityBadge[event.severity] ?? severityBadge.info;
  const time = new Date(event.timestamp).toLocaleString();

  return (
    <div className="flex items-start gap-2 rounded-lg border border-surface-border bg-card p-2.5">
      <SevIcon className={cn('mt-0.5 size-4 shrink-0', severityIconColor[event.severity] ?? 'text-muted-foreground')} />
      <div className="min-w-0 flex-1 space-y-0.5">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs font-medium text-foreground">
            {event.event_type}
          </span>
          <Badge variant={badge.variant} className={badge.className}>{event.severity}</Badge>
          {event.agent_id && (
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <ActorAvatar actorType="agent" name={event.agent_id} size="xs" />
              {event.agent_id}
            </span>
          )}
        </div>
        <p className="truncate font-mono text-xs text-muted-foreground">{time}</p>
      </div>
    </div>
  );
}

function SecurityCard({
  icon: Icon,
  title,
  description,
  children,
  className,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  description: string;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <Card className={className}>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-sm font-medium">
          <Icon className="size-4 text-brand" />
          {title}
        </CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

function StatusRow({
  label,
  status,
  value,
}: {
  label: string;
  status?: string;
  value?: string;
}) {
  const intl = useIntl();
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-muted-foreground">{label}</span>
      {status === 'active' ? (
        <Badge variant="secondary" className="bg-success/15 text-success">
          <Shield className="size-3" />
          {intl.formatMessage({ id: 'security.active' })}
        </Badge>
      ) : status === 'inactive' ? (
        <Badge variant="outline">{intl.formatMessage({ id: 'security.inactive' })}</Badge>
      ) : (
        <span className="font-mono text-sm font-medium text-foreground">
          {value}
        </span>
      )}
    </div>
  );
}

function RuleRow({ path, access }: { path: string; access: string }) {
  const accessBadge: Record<string, { variant: 'secondary' | 'destructive'; className?: string }> = {
    rw: { variant: 'secondary', className: 'bg-success/15 text-success' },
    ro: { variant: 'secondary', className: 'bg-warning/15 text-warning' },
    deny: { variant: 'destructive' },
  };
  const badge = accessBadge[access] ?? { variant: 'secondary' as const };

  return (
    <div className="flex items-center justify-between text-sm">
      <code className="rounded bg-muted px-2 py-0.5 font-mono text-xs text-foreground">
        {path}
      </code>
      <Badge variant={badge.variant} className={badge.className}>{access}</Badge>
    </div>
  );
}
