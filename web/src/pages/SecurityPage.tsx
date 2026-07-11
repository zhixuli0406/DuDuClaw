import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { api, type AuditEvent, type KillswitchConfig } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { FormField } from '@/components/shared/Dialog';
import { AdvancedSection } from '@/components/settings/controls';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import {
  Page,
  PageHeader,
  Card,
  Section,
  Badge,
  Button,
  EmptyState,
  Mono,
  CharacterAvatar,
  controlClass,
} from '@/components/ui';
import { cn } from '@/lib/utils';
import {
  Shield,
  Lock,
  ShieldCheck,
  Users,
  Timer,
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
    <Page wide>
      <PageHeader
        icon={Shield}
        title={intl.formatMessage({ id: 'nav.security' })}
        subtitle={intl.formatMessage({ id: 'app.subtitle' })}
      />

      <div className="grid gap-6 lg:grid-cols-2">
        {/* Audit Log */}
        <SecurityCard
          icon={History}
          title={intl.formatMessage({ id: 'security.audit.title' })}
          description={intl.formatMessage({ id: 'security.audit.desc' })}
        >
          {auditLoading ? (
            <p className="py-4 text-center text-sm text-stone-400">
              {intl.formatMessage({ id: 'common.loading' })}
            </p>
          ) : auditEvents.length === 0 ? (
            <EmptyState
              icon={History}
              dudu="curious"
              title={intl.formatMessage({ id: 'security.audit.empty' })}
            />
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
              status={status?.credential_proxy.active ? 'active' : 'inactive'}
            />
            <StatusRow
              label={intl.formatMessage({ id: 'security.vaultBackend' })}
              value={status?.credential_proxy.vault_backend ?? '—'}
            />
            <StatusRow
              label={intl.formatMessage({ id: 'security.injectedSecrets' })}
              value={String(status?.credential_proxy.injected_secrets ?? 0)}
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
            {status?.mount_guard.rules && status.mount_guard.rules.length > 0 ? (
              status.mount_guard.rules.map((rule) => (
                <RuleRow key={rule.path} path={rule.path} access={rule.access} />
              ))
            ) : (
              <p className="py-2 text-center text-sm text-stone-400">
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
        >
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)]">
                  <th className="py-2 text-left font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'security.rbac.agent' })}
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'security.rbac.tool' })}
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'security.rbac.web' })}
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'security.rbac.file' })}
                  </th>
                  <th className="py-2 text-center font-medium text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'security.rbac.shell' })}
                  </th>
                </tr>
              </thead>
              <tbody className="text-stone-700 dark:text-stone-300">
                {(status?.rbac ?? []).map((agent) => (
                  <tr key={agent.agent_id} className="border-b border-[var(--panel-border)]">
                    <td className="py-2 text-stone-700 dark:text-stone-300">
                      <span className="flex items-center gap-2">
                        <CharacterAvatar agentId={agent.agent_id} name={agent.agent_id} size={24} />
                        <Mono className="text-xs">{agent.agent_id}</Mono>
                        <span className="text-xs text-stone-400">({agent.role})</span>
                      </span>
                    </td>
                    <PermCell allowed={agent.tool_use} />
                    <PermCell allowed={agent.web_access} />
                    <PermCell allowed={agent.file_write} />
                    <PermCell allowed={agent.shell_exec} />
                  </tr>
                ))}
                {(!status?.rbac || status.rbac.length === 0) && (
                  <tr>
                    <td colSpan={5} className="py-4 text-center text-sm text-stone-400">
                      {intl.formatMessage({ id: 'common.noData' })}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </SecurityCard>

        {/* Rate Limiter */}
        <SecurityCard
          icon={Timer}
          title={intl.formatMessage({ id: 'security.rateLimiter.title' })}
          description={intl.formatMessage({ id: 'security.rateLimiter.desc' })}
        >
          <div className="space-y-3">
            <LimitRow
              label={intl.formatMessage({ id: 'security.reqPerMin' })}
              value={String(status?.rate_limiter.requests_per_minute ?? 60)}
            />
            <LimitRow
              label={intl.formatMessage({ id: 'security.concurrent' })}
              value={String(status?.rate_limiter.concurrent_requests ?? 5)}
            />
          </div>
        </SecurityCard>
      </div>

      {/* Killswitch (KS) — editable */}
      <KillswitchSection />
    </Page>
  );
}

// ── Killswitch editor (KS) ──────────────────────────────────────

function KillswitchSection() {
  const intl = useIntl();
  const connectionState = useConnectionStore((s) => s.state);
  const [config, setConfig] = useState<KillswitchConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
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

  return (
    <SecurityCard
      icon={OctagonX}
      title={intl.formatMessage({ id: 'killswitch.title' })}
      description={intl.formatMessage({ id: 'killswitch.desc' })}
    >
      {!config ? (
        <p className="py-4 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
      ) : (
        <div className="space-y-6">
          {/* Triggers */}
          <Section title={intl.formatMessage({ id: 'killswitch.triggers' })}>
            <div className="grid gap-4 sm:grid-cols-2">
              <FormField label={intl.formatMessage({ id: 'killswitch.maxRepliesPerMinute' })} hint={intl.formatMessage({ id: 'killswitch.maxRepliesPerMinute.help' })}>
                <input type="number" min={1} max={10000} value={config.triggers.max_replies_per_minute} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, max_replies_per_minute: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.maxConsecutiveErrors' })} hint={intl.formatMessage({ id: 'killswitch.maxConsecutiveErrors.help' })}>
                <input type="number" min={1} max={1000} value={config.triggers.max_consecutive_errors} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, max_consecutive_errors: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.errorRateThreshold' })} hint={intl.formatMessage({ id: 'killswitch.errorRateThreshold.help' })}>
                <input type="number" min={0} max={1} step={0.01} value={config.triggers.error_rate_threshold} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, error_rate_threshold: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.costLimitUsd' })} hint={intl.formatMessage({ id: 'killswitch.costLimitUsd.help' })}>
                <input type="number" min={0} step={0.01} value={config.triggers.cost_limit_usd} onChange={(e) => setConfig({ ...config, triggers: { ...config.triggers, cost_limit_usd: Number(e.target.value) } })} className={controlClass} />
              </FormField>
            </div>
          </Section>

          {/* Circuit breaker — advanced thresholds */}
          <AdvancedSection storageKey="security.killswitch" label={intl.formatMessage({ id: 'killswitch.advanced' })}>
            <Section title={intl.formatMessage({ id: 'killswitch.circuitBreaker' })}>
            <div className="grid gap-4 sm:grid-cols-2">
              <FormField label={intl.formatMessage({ id: 'killswitch.frequencyWindowSecs' })} hint="1-86400">
                <input type="number" min={1} max={86400} value={config.circuit_breaker.frequency_window_secs} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, frequency_window_secs: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.frequencyMaxReplies' })} hint="1-10000">
                <input type="number" min={1} max={10000} value={config.circuit_breaker.frequency_max_replies} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, frequency_max_replies: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.similarityThreshold' })} hint="0.0-1.0">
                <input type="number" min={0} max={1} step={0.01} value={config.circuit_breaker.similarity_threshold} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, similarity_threshold: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.tokenExplosionMultiplier' })} hint="1.0-1000.0">
                <input type="number" min={1} max={1000} step={0.1} value={config.circuit_breaker.token_explosion_multiplier} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, token_explosion_multiplier: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.cooldownSecs' })} hint="0-86400">
                <input type="number" min={0} max={86400} value={config.circuit_breaker.cooldown_secs} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, cooldown_secs: Number(e.target.value) } })} className={controlClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'killswitch.halfOpenAllowCount' })} hint="1-1000">
                <input type="number" min={1} max={1000} value={config.circuit_breaker.half_open_allow_count} onChange={(e) => setConfig({ ...config, circuit_breaker: { ...config.circuit_breaker, half_open_allow_count: Number(e.target.value) } })} className={controlClass} />
              </FormField>
            </div>
            </Section>
          </AdvancedSection>

          {/* Safety words */}
          <Section
            title={intl.formatMessage({ id: 'killswitch.safetyWords' })}
            className="border-t border-[var(--panel-border)] pt-4"
          >
            <div className="space-y-4">
              {(['stop', 'stop_all', 'resume', 'status'] as const).map((key) => (
                <FormField key={key} label={intl.formatMessage({ id: `killswitch.safetyWords.${key}` })}>
                  <ChipEditor
                    values={config.safety_words[key]}
                    onChange={(v) => setConfig({ ...config, safety_words: { ...config.safety_words, [key]: v } })}
                    placeholder={key}
                    addLabel={intl.formatMessage({ id: 'common.add' })}
                  />
                </FormField>
              ))}
            </div>
          </Section>

          {/* Defensive prompt */}
          <Section
            title={intl.formatMessage({ id: 'killswitch.defensivePrompt' })}
            className="border-t border-[var(--panel-border)] pt-4"
          >
            <label className="flex items-center justify-between py-1.5">
              <span className="text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'killswitch.defensivePrompt.enabled' })}</span>
              <input type="checkbox" checked={config.defensive_prompt.enabled} onChange={(e) => setConfig({ ...config, defensive_prompt: { ...config.defensive_prompt, enabled: e.target.checked } })} className="h-4 w-4 accent-amber-500" />
            </label>
            <FormField label={intl.formatMessage({ id: 'killswitch.defensivePrompt.languages' })}>
              <ChipEditor
                values={config.defensive_prompt.languages}
                onChange={(v) => setConfig({ ...config, defensive_prompt: { ...config.defensive_prompt, languages: v } })}
                placeholder="zh-TW"
                addLabel={intl.formatMessage({ id: 'common.add' })}
              />
            </FormField>
          </Section>

          <div className="flex justify-end gap-2 pt-2">
            {saved && <span className="self-center text-xs text-emerald-600 dark:text-emerald-400">{intl.formatMessage({ id: 'settings.general.saved' })}</span>}
            <Button variant="primary" onClick={handleSave} disabled={saving}>
              {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
            </Button>
          </div>
        </div>
      )}
    </SecurityCard>
  );
}

function PermCell({ allowed }: { allowed: boolean }) {
  return (
    <td className="py-2 text-center">
      {allowed ? (
        <span className="text-emerald-500">&#10003;</span>
      ) : (
        <span className="text-stone-300 dark:text-stone-600">&#10005;</span>
      )}
    </td>
  );
}

function AuditRow({ event }: { event: AuditEvent }) {
  const severityStyles: Record<string, string> = {
    info: 'text-sky-500',
    warning: 'text-amber-500',
    critical: 'text-rose-500',
  };

  const SevIcon = event.severity === 'critical' ? AlertTriangle
    : event.severity === 'warning' ? FileWarning
    : Shield;

  const time = new Date(event.timestamp).toLocaleString();

  return (
    <div className="flex items-start gap-2 rounded-control border border-[var(--panel-border)] bg-[var(--panel-fill)] p-2.5">
      <SevIcon className={cn('mt-0.5 h-4 w-4 shrink-0', severityStyles[event.severity] ?? 'text-stone-400')} />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-stone-900 dark:text-stone-100">
            {event.event_type}
          </span>
          {event.agent_id && (
            <span className="flex items-center gap-1 text-xs text-stone-400">
              <CharacterAvatar agentId={event.agent_id} name={event.agent_id} size={16} />
              {event.agent_id}
            </span>
          )}
        </div>
        <Mono className="block truncate text-xs">{time}</Mono>
      </div>
    </div>
  );
}

function SecurityCard({
  icon: Icon,
  title,
  description,
  children,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <Icon className="h-4 w-4 text-amber-500" />
          {title}
        </span>
      }
    >
      <p className="mb-4 text-xs text-stone-500 dark:text-stone-400">{description}</p>
      {children}
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
      <span className="text-stone-600 dark:text-stone-400">{label}</span>
      {status === 'active' ? (
        <Badge tone="success">
          <Shield className="h-3 w-3" />
          {intl.formatMessage({ id: 'security.active' })}
        </Badge>
      ) : status === 'inactive' ? (
        <Badge tone="neutral">{intl.formatMessage({ id: 'security.inactive' })}</Badge>
      ) : (
        <Mono className="font-medium text-stone-900 dark:text-stone-50">
          {value}
        </Mono>
      )}
    </div>
  );
}

function RuleRow({ path, access }: { path: string; access: string }) {
  const accessTone: Record<string, 'success' | 'warning' | 'danger'> = {
    rw: 'success',
    ro: 'warning',
    deny: 'danger',
  };

  return (
    <div className="flex items-center justify-between text-sm">
      <Mono className="rounded-control bg-stone-500/10 px-2 py-0.5 text-xs text-stone-700 dark:text-stone-300">
        {path}
      </Mono>
      <Badge tone={accessTone[access] ?? 'neutral'}>{access}</Badge>
    </div>
  );
}

function LimitRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between text-sm">
      <span className="text-stone-600 dark:text-stone-400">{label}</span>
      <Mono className="font-medium text-stone-900 dark:text-stone-50">
        {value}
      </Mono>
    </div>
  );
}
