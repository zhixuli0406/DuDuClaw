import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type RedactionConfig,
  type RedactionSourceMode,
  type RedactionSources,
  type RedactionRestoreArgs,
  type RedactionEgressRule,
  type RedactionUpdate,
  type RedactionStats,
  type RedactionPolicyStatus,
  type RedactionAuditEntry,
} from '@/lib/api';
import { FormField, inputClass, selectClass } from '@/components/shared/Dialog';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { toast, formatError } from '@/lib/toast';
import { Card, Button, Badge } from '@/components/ui';
import {
  EyeOff,
  Plus,
  Trash2,
  ScrollText,
  ShieldCheck,
  RefreshCw,
  AlertTriangle,
  ChevronDown,
  Database,
  Info,
} from 'lucide-react';

// ── Privacy / Redaction Tab (RED) ──────────────────────────────
//
// This tab is written for a non-engineer operator. The redaction ENGINE speaks
// in `Source × Mode` matrices and `tool_egress` globs; here we translate that
// into plain "what data gets protected" + "which external systems" language, and
// tuck the raw knobs (vault TTL, purge, profiles) behind an "Advanced" fold.

// Sources reordered so the main protection point (tool results — i.e. every file /
// wiki / memory / ERP / CRM result the AI reads) leads.
const REDACTION_SOURCE_KEYS: ReadonlyArray<keyof RedactionSources> = [
  'tool_results',
  'user_input',
  'system_prompt',
  'sub_agent',
  'cron_context',
];
const REDACTION_MODES: ReadonlyArray<RedactionSourceMode> = ['on', 'off', 'selective', 'inherit'];
const REDACTION_RESTORE: ReadonlyArray<RedactionRestoreArgs> = ['deny', 'restore', 'passthrough'];

/**
 * Known external business systems → the tool-name prefix their MCP tools use.
 * Every tool RESULT is already de-identified by the `tool_results` source policy;
 * a `tool_egress` rule keyed by this prefix governs the reverse direction —
 * whether the AI may write REAL (restored) values BACK into that system.
 *
 * `connected` distinguishes a system with a shipping connector (Odoo) from a
 * template whose connector is still on the roadmap — we pre-arm the rule either
 * way (defense in depth: protection is already on the day the connector lands),
 * but we never pretend a live integration exists.
 */
interface ExternalSystemPreset {
  id: string;
  toolKey: string;
  labelId: string;
  connected: boolean;
}
const EXTERNAL_SYSTEM_PRESETS: ReadonlyArray<ExternalSystemPreset> = [
  { id: 'odoo', toolKey: 'odoo.*', labelId: 'redaction.ext.odoo', connected: true },
  { id: 'digiwin', toolKey: 'digiwin.*', labelId: 'redaction.ext.digiwin', connected: false },
  { id: 'salesforce', toolKey: 'salesforce.*', labelId: 'redaction.ext.salesforce', connected: false },
  { id: 'hubspot', toolKey: 'hubspot.*', labelId: 'redaction.ext.hubspot', connected: false },
];

function presetForKey(key: string): ExternalSystemPreset | undefined {
  return EXTERNAL_SYSTEM_PRESETS.find((p) => p.toolKey === key);
}

export function RedactionTab() {
  const intl = useIntl();
  const [config, setConfig] = useState<RedactionConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [newTool, setNewTool] = useState('');
  const [customOpen, setCustomOpen] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Keys the server currently holds. `redaction.update` merges tool_egress as an
  // upsert (a value of `null` removes; an absent key is left untouched), so on
  // save we must explicitly null out any key the operator deleted — otherwise a
  // removed external system silently reappears on the next load.
  const savedEgressKeysRef = useRef<string[]>([]);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    try {
      const res = await api.redaction.get();
      setConfig(res);
      savedEgressKeysRef.current = Object.keys(res.tool_egress);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    try {
      const egress: Record<string, RedactionEgressRule | null> = { ...config.tool_egress };
      for (const k of savedEgressKeysRef.current) {
        if (!(k in config.tool_egress)) egress[k] = null; // explicit removal
      }
      const payload: RedactionUpdate = {
        enabled: config.enabled,
        vault_ttl_hours: config.vault_ttl_hours,
        purge_after_expire_days: config.purge_after_expire_days,
        profiles: config.profiles,
        sources: config.sources,
        tool_egress: egress,
      };
      await api.redaction.update(payload);
      savedEgressKeysRef.current = Object.keys(config.tool_egress);
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  // Add an external system by its tool-name prefix. Default to the safest egress
  // policy (deny — never send real values back out) so a one-click add can only
  // tighten, never loosen, protection.
  const addEgressKey = (rawKey: string) => {
    const tool = rawKey.trim();
    if (!tool || !config || config.tool_egress[tool]) return;
    setConfig({
      ...config,
      tool_egress: { ...config.tool_egress, [tool]: { restore_args: 'deny', audit_reveal: false } },
    });
  };

  const addCustom = () => {
    addEgressKey(newTool);
    setNewTool('');
    setCustomOpen(false);
  };

  const removeEgress = (tool: string) => {
    if (!config) return;
    const next = { ...config.tool_egress };
    delete next[tool];
    setConfig({ ...config, tool_egress: next });
  };

  if (!config) {
    return (
      <Card>
        <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
      </Card>
    );
  }

  const egressEntries = Object.entries(config.tool_egress) as Array<[string, RedactionEgressRule]>;

  return (
    <div className="space-y-6">
    <Card
      bodyClassName="space-y-6"
      title={
        <span className="flex items-center gap-2">
          <EyeOff className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.redaction' })}
        </span>
      }
    >
      {/* Plain-language explainer: what this feature actually does. */}
      <div className="flex gap-2.5 rounded-lg bg-amber-500/5 px-3.5 py-3 text-xs leading-relaxed text-stone-600 dark:bg-amber-400/10 dark:text-stone-300">
        <Info className="mt-0.5 h-4 w-4 shrink-0 text-amber-500" />
        <span>{intl.formatMessage({ id: 'redaction.intro' })}</span>
      </div>

      {/* Master toggle */}
      <label className="flex items-center justify-between py-1.5">
        <span className="text-sm font-medium text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'redaction.enabled' })}</span>
        <input type="checkbox" checked={config.enabled} onChange={(e) => setConfig({ ...config, enabled: e.target.checked })} className="h-4 w-4 accent-amber-500" />
      </label>

      {/* Sources — plain title + one-line description per row */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-1 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'redaction.sources' })}</h4>
        <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.sources.hint' })}</p>
        <div className="space-y-3">
          {REDACTION_SOURCE_KEYS.map((key) => (
            <div key={key} className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <p className="text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: `redaction.source.${key}` })}</p>
                <p className="mt-0.5 text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: `redaction.source.${key}.desc` })}</p>
              </div>
              <select
                value={config.sources[key]}
                onChange={(e) => setConfig({ ...config, sources: { ...config.sources, [key]: e.target.value as RedactionSourceMode } })}
                className={cn(selectClass, 'w-32 shrink-0')}
              >
                {REDACTION_MODES.map((m) => (
                  <option key={m} value={m}>{intl.formatMessage({ id: `redaction.mode.${m}` })}</option>
                ))}
              </select>
            </div>
          ))}
        </div>
      </div>

      {/* External systems (ERP / CRM / database) — friendly wrapper over tool_egress */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-1 flex items-center gap-1.5 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">
          <Database className="h-3.5 w-3.5" />
          {intl.formatMessage({ id: 'redaction.ext.title' })}
        </h4>
        <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.ext.desc' })}</p>

        {/* One-click presets */}
        <div className="mb-3 flex flex-wrap gap-2">
          {EXTERNAL_SYSTEM_PRESETS.map((p) => {
            const added = !!config.tool_egress[p.toolKey];
            return (
              <button
                key={p.id}
                onClick={() => addEgressKey(p.toolKey)}
                disabled={added}
                className={cn(
                  'inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs font-medium transition-colors',
                  added
                    ? 'cursor-not-allowed border-stone-200 text-stone-300 dark:border-stone-700 dark:text-stone-600'
                    : 'border-amber-300 text-amber-700 hover:bg-amber-500/10 dark:border-amber-500/40 dark:text-amber-400',
                )}
              >
                <Plus className="h-3 w-3" />
                {intl.formatMessage({ id: p.labelId })}
              </button>
            );
          })}
          <button
            onClick={() => setCustomOpen((v) => !v)}
            className="inline-flex items-center gap-1.5 rounded-full border border-stone-300 px-3 py-1 text-xs font-medium text-stone-600 hover:bg-stone-500/10 dark:border-stone-600 dark:text-stone-300"
          >
            <Plus className="h-3 w-3" />
            {intl.formatMessage({ id: 'redaction.ext.custom' })}
          </button>
        </div>

        {/* Custom tool-prefix input (revealed on demand) */}
        {customOpen && (
          <div className="mb-3 flex gap-2">
            <input
              type="text"
              value={newTool}
              onChange={(e) => setNewTool(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') addCustom(); }}
              placeholder={intl.formatMessage({ id: 'redaction.ext.customPlaceholder' })}
              className={cn(inputClass, 'flex-1')}
            />
            <Button variant="secondary" icon={Plus} onClick={addCustom}>
              {intl.formatMessage({ id: 'common.add' })}
            </Button>
          </div>
        )}

        {/* Configured systems */}
        <div className="space-y-2">
          {egressEntries.map(([tool, rule]) => {
            const preset = presetForKey(tool);
            const name = preset ? intl.formatMessage({ id: preset.labelId }) : tool;
            return (
              <div key={tool} className="flex flex-wrap items-center gap-2 rounded-lg bg-stone-500/5 p-2.5 dark:bg-white/5">
                <span className="flex items-center gap-1.5 text-sm font-medium text-stone-700 dark:text-stone-300">
                  {name}
                  {preset && (
                    <Badge tone={preset.connected ? 'success' : 'neutral'}>
                      {intl.formatMessage({ id: preset.connected ? 'redaction.ext.connected' : 'redaction.ext.template' })}
                    </Badge>
                  )}
                </span>
                {!preset && (
                  <code className="rounded bg-stone-500/10 px-1.5 py-0.5 font-mono text-xs text-stone-500 dark:text-stone-400">{tool}</code>
                )}
                <div className="ml-auto flex flex-wrap items-center gap-2">
                  <label className="flex items-center gap-1.5 text-xs text-stone-500 dark:text-stone-400">
                    {intl.formatMessage({ id: 'redaction.ext.egressPolicy' })}
                    <select
                      value={rule.restore_args}
                      onChange={(e) => setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { ...rule, restore_args: e.target.value as RedactionRestoreArgs } } })}
                      className={cn(selectClass, 'w-44')}
                    >
                      {REDACTION_RESTORE.map((r) => (
                        <option key={r} value={r}>{intl.formatMessage({ id: `redaction.restore.${r}` })}</option>
                      ))}
                    </select>
                  </label>
                  <label className="flex items-center gap-1.5 text-xs text-stone-600 dark:text-stone-400">
                    <input type="checkbox" checked={rule.audit_reveal} onChange={(e) => setConfig({ ...config, tool_egress: { ...config.tool_egress, [tool]: { ...rule, audit_reveal: e.target.checked } } })} className="accent-amber-500" />
                    {intl.formatMessage({ id: 'redaction.auditReveal' })}
                  </label>
                  <button onClick={() => removeEgress(tool)} title={intl.formatMessage({ id: 'common.remove' })} className="rounded p-1 text-rose-500 hover:bg-rose-500/10">
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
            );
          })}
          {egressEntries.length === 0 && (
            <p className="rounded-lg bg-stone-500/5 px-3 py-4 text-center text-xs text-stone-400 dark:bg-white/5">{intl.formatMessage({ id: 'redaction.ext.empty' })}</p>
          )}
        </div>
      </div>

      {/* Advanced — the raw retention / profile knobs, folded away by default */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <button
          onClick={() => setShowAdvanced((v) => !v)}
          className="flex w-full items-center justify-between text-xs font-semibold uppercase text-stone-500 hover:text-stone-700 dark:text-stone-400 dark:hover:text-stone-200"
        >
          {intl.formatMessage({ id: 'redaction.advanced' })}
          <ChevronDown className={cn('h-4 w-4 transition-transform', showAdvanced && 'rotate-180')} />
        </button>
        {showAdvanced && (
          <div className="mt-4 space-y-4">
            <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.advanced.hint' })}</p>
            <div className="grid gap-4 sm:grid-cols-2">
              <FormField label={intl.formatMessage({ id: 'redaction.vaultTtl' })} hint="1-8760">
                <input type="number" min={1} max={8760} value={config.vault_ttl_hours} onChange={(e) => setConfig({ ...config, vault_ttl_hours: Number(e.target.value) })} className={inputClass} />
              </FormField>
              <FormField label={intl.formatMessage({ id: 'redaction.purgeAfter' })} hint="0-3650">
                <input type="number" min={0} max={3650} value={config.purge_after_expire_days} onChange={(e) => setConfig({ ...config, purge_after_expire_days: Number(e.target.value) })} className={inputClass} />
              </FormField>
            </div>
            <FormField label={intl.formatMessage({ id: 'redaction.profiles' })} hint={intl.formatMessage({ id: 'redaction.profiles.hint' })}>
              <ChipEditor values={config.profiles} onChange={(v) => setConfig({ ...config, profiles: v })} placeholder="pii" addLabel={intl.formatMessage({ id: 'common.add' })} />
            </FormField>
          </div>
        )}
      </div>

      <div className="flex items-center justify-end gap-2 pt-2">
        {saved && <span className="text-xs text-emerald-600 dark:text-emerald-400">{intl.formatMessage({ id: 'settings.general.saved' })}</span>}
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </Card>

    <RedactionAuditSection />
    </div>
  );
}

// ── Audit / stats view (read-only) ─────────────────────────────

/** Human-readable label for a redaction audit event tag. Falls back to the
 *  raw tag so unknown event types still render meaningfully. */
function auditEventLabel(intl: ReturnType<typeof useIntl>, event: string): string {
  const id = `redaction.audit.event.${event}`;
  const msg = intl.formatMessage({ id, defaultMessage: event });
  return msg;
}

function auditEventTone(event: string): 'success' | 'warning' | 'danger' | 'neutral' {
  switch (event) {
    case 'restore_ok':
    case 'egress_allow':
      return 'success';
    case 'restore_denied':
    case 'egress_deny':
      return 'danger';
    case 'restore_miss':
    case 'force_on_override':
      return 'warning';
    default:
      return 'neutral';
  }
}

function RedactionAuditSection() {
  const intl = useIntl();
  const [stats, setStats] = useState<RedactionStats | null>(null);
  const [policy, setPolicy] = useState<RedactionPolicyStatus | null>(null);
  const [audit, setAudit] = useState<RedactionAuditEntry[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  const load = useCallback(async (silent = false) => {
    if (silent) setRefreshing(true);
    else setLoading(true);
    setError(null);
    try {
      const [s, p, a] = await Promise.all([
        api.redaction.stats(),
        api.redaction.policyStatus(),
        api.redaction.recentAudit(50),
      ]);
      setStats(s);
      setPolicy(p);
      setAudit(a.entries ?? []);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const enabled = policy?.config_enabled ?? stats?.config_enabled ?? false;

  return (
    <Card
      bodyClassName="space-y-5"
      title={
        <span className="flex items-center justify-between gap-2">
          <span className="flex items-center gap-2">
            <ScrollText className="h-4 w-4 text-amber-500" />
            {intl.formatMessage({ id: 'redaction.audit.title' })}
          </span>
          <button
            onClick={() => load(true)}
            disabled={loading || refreshing}
            title={intl.formatMessage({ id: 'common.refresh' })}
            className={cn(
              'rounded p-1 text-stone-400 transition-colors hover:text-stone-700 dark:hover:text-stone-200',
              refreshing && '[&_svg]:animate-spin',
            )}
          >
            <RefreshCw className="h-3.5 w-3.5" />
          </button>
        </span>
      }
    >
      <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.audit.desc' })}</p>

      {loading ? (
        <div className="space-y-2">
          {[0, 1, 2].map((i) => (
            <div key={i} className="h-10 animate-pulse rounded-lg bg-stone-500/5 dark:bg-white/5" />
          ))}
        </div>
      ) : error ? (
        <div className="flex items-center gap-2 rounded-lg border border-rose-200 bg-rose-50 px-3 py-2.5 text-sm text-rose-700 dark:border-rose-800/50 dark:bg-rose-900/20 dark:text-rose-400">
          <AlertTriangle className="h-4 w-4 shrink-0" />
          <span>{intl.formatMessage({ id: 'redaction.audit.error' }, { message: error })}</span>
        </div>
      ) : (
        <>
          {/* Policy status pills */}
          <div className="flex flex-wrap items-center gap-2">
            <Badge tone={enabled ? 'success' : 'neutral'} dot>
              {enabled
                ? intl.formatMessage({ id: 'redaction.audit.policyOn' })
                : intl.formatMessage({ id: 'redaction.audit.policyOff' })}
            </Badge>
            {policy && (
              <>
                <Badge tone="neutral">
                  {intl.formatMessage({ id: 'redaction.audit.ruleCount' }, { count: policy.rule_count })}
                </Badge>
                <Badge tone="neutral">
                  {intl.formatMessage({ id: 'redaction.audit.ttl' }, { hours: policy.vault_ttl_hours })}
                </Badge>
                {policy.override_active && (
                  <Badge tone="warning" dot>
                    {intl.formatMessage({ id: 'redaction.audit.overrideActive' })}
                  </Badge>
                )}
              </>
            )}
          </div>

          {/* Vault counters */}
          {stats && (
            <div className="grid grid-cols-3 gap-3">
              <VaultStat label={intl.formatMessage({ id: 'redaction.audit.stat.total' })} value={stats.vault.total} />
              <VaultStat label={intl.formatMessage({ id: 'redaction.audit.stat.active' })} value={stats.vault.active} tone="emerald" />
              <VaultStat label={intl.formatMessage({ id: 'redaction.audit.stat.expired' })} value={stats.vault.expired} tone="stone" />
            </div>
          )}

          {/* Masked PII categories */}
          <div>
            <h4 className="mb-2 flex items-center gap-1.5 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">
              <ShieldCheck className="h-3.5 w-3.5" />
              {intl.formatMessage({ id: 'redaction.audit.categories' })}
            </h4>
            {stats && stats.vault.by_category.length > 0 ? (
              <div className="flex flex-wrap gap-2">
                {stats.vault.by_category.map(([cat, count]) => (
                  <span
                    key={cat}
                    className="inline-flex items-center gap-1.5 rounded-full bg-amber-500/10 px-2.5 py-1 text-xs font-medium text-amber-700 dark:text-amber-400"
                  >
                    {cat}
                    <span className="rounded-full bg-amber-500/20 px-1.5 tabular-nums">{count}</span>
                  </span>
                ))}
              </div>
            ) : (
              <p className="text-xs text-stone-400">{intl.formatMessage({ id: 'redaction.audit.noCategories' })}</p>
            )}
          </div>

          {/* Recent audit records */}
          <div>
            <h4 className="mb-2 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">
              {intl.formatMessage({ id: 'redaction.audit.recent' })}
            </h4>
            {audit && audit.length > 0 ? (
              <ul className="space-y-1.5">
                {audit.map((entry, i) => (
                  <li
                    key={i}
                    className="flex flex-wrap items-center gap-2 rounded-lg bg-stone-500/5 px-3 py-2 text-xs dark:bg-white/5"
                  >
                    <Badge tone={auditEventTone(entry.event)}>{auditEventLabel(intl, entry.event)}</Badge>
                    {entry.category && (
                      <code className="rounded bg-stone-500/10 px-1.5 py-0.5 font-mono text-stone-600 dark:text-stone-300">{entry.category}</code>
                    )}
                    {entry.tool && (
                      <code className="rounded bg-stone-500/10 px-1.5 py-0.5 font-mono text-stone-600 dark:text-stone-300">{entry.tool}</code>
                    )}
                    {entry.agent_id && <span className="text-stone-500 dark:text-stone-400">{entry.agent_id}</span>}
                    {entry.ts && (
                      <span className="ml-auto font-mono text-stone-400 dark:text-stone-500">
                        {new Date(entry.ts).toLocaleString('zh-TW', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="rounded-lg bg-stone-500/5 px-3 py-4 text-center text-xs text-stone-400 dark:bg-white/5">
                {intl.formatMessage({ id: 'redaction.audit.noRecords' })}
              </p>
            )}
          </div>
        </>
      )}
    </Card>
  );
}

function VaultStat({ label, value, tone = 'amber' }: { label: string; value: number; tone?: 'amber' | 'emerald' | 'stone' }) {
  const color =
    tone === 'emerald'
      ? 'text-emerald-600 dark:text-emerald-400'
      : tone === 'stone'
        ? 'text-stone-500 dark:text-stone-400'
        : 'text-amber-600 dark:text-amber-400';
  return (
    <div className="rounded-lg bg-stone-500/5 px-3 py-2.5 dark:bg-white/5">
      <p className={cn('text-xl font-bold tabular-nums', color)}>{value.toLocaleString()}</p>
      <p className="mt-0.5 text-xs text-stone-500 dark:text-stone-400">{label}</p>
    </div>
  );
}
