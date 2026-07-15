import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type RedactionConfig,
  type RedactionSourceMode,
  type RedactionSourceSetting,
  type RedactionSources,
  type RedactionRestoreArgs,
  type RedactionEgressRule,
  type RedactionUpdate,
  type RedactionStats,
  type RedactionPolicyStatus,
  type RedactionAuditEntry,
} from '@/lib/api';
import { FormField, inputClass, selectClass } from '@/components/shared/Dialog';
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

/** Human label for a PII category — i18n when we know it, raw tag otherwise. */
function categoryLabel(intl: ReturnType<typeof useIntl>, cat: string): string {
  return intl.formatMessage({ id: `redaction.cat.${cat}`, defaultMessage: cat });
}

/** Which field-filter shape a source setting is currently in. */
type FieldScope = 'all' | 'only' | 'exclude';
function scopeOf(s: RedactionSourceSetting): FieldScope {
  if (s.only_categories.length > 0) return 'only';
  if (s.exclude_categories.length > 0) return 'exclude';
  return 'all';
}

/** One source row: mode select + expandable per-field scope editor. */
function SourceSettingRow({
  sourceKey,
  setting,
  categories,
  onChange,
}: {
  sourceKey: keyof RedactionSources;
  setting: RedactionSourceSetting;
  /** Union of categories covered by the currently selected profiles. */
  categories: string[];
  onChange: (next: RedactionSourceSetting) => void;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const scope = scopeOf(setting);
  // The field filter only matters when this source actually redacts.
  const filterable =
    setting.mode === 'on' || (sourceKey === 'system_prompt' && setting.mode === 'selective');
  const activeList = scope === 'only' ? setting.only_categories : setting.exclude_categories;

  const setScope = (next: FieldScope) => {
    if (next === scope) return;
    // Carry the picked set across radio switches; 'all' clears both lists.
    const picked = activeList;
    onChange({
      ...setting,
      only_categories: next === 'only' ? picked : [],
      exclude_categories: next === 'exclude' ? picked : [],
    });
  };

  const toggleCategory = (cat: string) => {
    if (scope === 'all') return;
    const field = scope === 'only' ? 'only_categories' : 'exclude_categories';
    const list = setting[field];
    const next = list.includes(cat) ? list.filter((c) => c !== cat) : [...list, cat];
    onChange({ ...setting, [field]: next });
  };

  // Categories referenced in config but absent from the current profile
  // selection (e.g. a profile was unticked) — keep them visible so the saved
  // filter is never silently hidden.
  const orphaned = activeList.filter((c) => !categories.includes(c));
  const allCategories = [...categories, ...orphaned];

  return (
    <div>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm text-stone-700 dark:text-stone-300">
            {intl.formatMessage({ id: `redaction.source.${sourceKey}` })}
          </p>
          <p className="mt-0.5 text-xs text-stone-400 dark:text-stone-500">
            {intl.formatMessage({ id: `redaction.source.${sourceKey}.desc` })}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {filterable && (
            <button
              onClick={() => setOpen((v) => !v)}
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-stone-500 hover:bg-stone-500/10 dark:text-stone-400"
              aria-expanded={open}
            >
              {scope === 'all'
                ? intl.formatMessage({ id: 'redaction.fields.all' })
                : intl.formatMessage(
                    { id: `redaction.fields.${scope}.badge` },
                    { count: activeList.length },
                  )}
              <ChevronDown className={cn('h-3 w-3 transition-transform', open && 'rotate-180')} />
            </button>
          )}
          <select
            value={setting.mode}
            onChange={(e) => onChange({ ...setting, mode: e.target.value as RedactionSourceMode })}
            className={cn(selectClass, 'w-32')}
          >
            {REDACTION_MODES.map((m) => (
              <option key={m} value={m}>{intl.formatMessage({ id: `redaction.mode.${m}` })}</option>
            ))}
          </select>
        </div>
      </div>

      {filterable && open && (
        <div className="mt-2 rounded-lg bg-stone-500/5 p-3 dark:bg-white/5">
          <div className="mb-2 flex flex-wrap gap-3">
            {(['all', 'only', 'exclude'] as const).map((s) => (
              <label key={s} className="flex items-center gap-1.5 text-xs text-stone-600 dark:text-stone-300">
                <input
                  type="radio"
                  name={`fields-${sourceKey}`}
                  checked={scope === s}
                  onChange={() => setScope(s)}
                  className="accent-amber-500"
                />
                {intl.formatMessage({ id: `redaction.fields.${s}` })}
              </label>
            ))}
          </div>
          {scope !== 'all' && (
            allCategories.length > 0 ? (
              <div className="flex flex-wrap gap-1.5">
                {allCategories.map((cat) => {
                  const checked = activeList.includes(cat);
                  return (
                    <button
                      key={cat}
                      onClick={() => toggleCategory(cat)}
                      className={cn(
                        'rounded-full border px-2.5 py-1 text-xs font-medium transition-colors',
                        checked
                          ? 'border-amber-400 bg-amber-500/15 text-amber-700 dark:border-amber-500/60 dark:text-amber-400'
                          : 'border-stone-300 text-stone-500 hover:bg-stone-500/10 dark:border-stone-600 dark:text-stone-400',
                      )}
                      aria-pressed={checked}
                    >
                      {categoryLabel(intl, cat)}
                    </button>
                  );
                })}
              </div>
            ) : (
              <p className="text-xs text-stone-400">
                {intl.formatMessage({ id: 'redaction.fields.noneAvailable' })}
              </p>
            )
          )}
          {scope !== 'all' && activeList.length === 0 && (
            <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
              {intl.formatMessage({ id: `redaction.fields.${scope}.emptyHint` })}
            </p>
          )}
        </div>
      )}
    </div>
  );
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
      const res = await api.redaction.update(payload);
      savedEgressKeysRef.current = Object.keys(config.tool_egress);
      if (res.warning) {
        // Saved to disk but NOT live — say so instead of pretending success.
        toast.error(res.warning);
      } else {
        setSaved(true);
        savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
      }
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
  // Fields the current profile selection can recognise — feeds every source
  // row's per-field scope picker.
  const selectedCategories = Array.from(
    new Set(
      (config.available_profiles ?? [])
        .filter((p) => config.profiles.includes(p.name))
        .flatMap((p) => p.categories),
    ),
  ).sort();

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

      {/* Detection rule sets (profiles) — what fields CAN be recognised */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-1 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'redaction.profiles.title' })}</h4>
        <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.profiles.desc' })}</p>
        <div className="space-y-2">
          {(config.available_profiles ?? []).map((p) => {
            const checked = config.profiles.includes(p.name);
            return (
              <label key={p.name} className="flex cursor-pointer items-start gap-2.5 rounded-lg bg-stone-500/5 p-2.5 dark:bg-white/5">
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={() =>
                    setConfig({
                      ...config,
                      profiles: checked
                        ? config.profiles.filter((n) => n !== p.name)
                        : [...config.profiles, p.name],
                    })
                  }
                  className="mt-0.5 h-4 w-4 accent-amber-500"
                />
                <span className="min-w-0 flex-1">
                  <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium text-stone-700 dark:text-stone-300">
                    {intl.formatMessage({ id: `redaction.profile.${p.name}`, defaultMessage: p.name })}
                    {!p.builtin && <Badge tone="neutral">{intl.formatMessage({ id: 'redaction.profile.custom' })}</Badge>}
                  </span>
                  <span className="mt-1 flex flex-wrap gap-1">
                    {p.categories.map((cat) => (
                      <span key={cat} className="rounded-full bg-stone-500/10 px-2 py-0.5 text-[11px] text-stone-500 dark:text-stone-400">
                        {categoryLabel(intl, cat)}
                      </span>
                    ))}
                  </span>
                </span>
              </label>
            );
          })}
          {/* Config may reference profiles missing from the catalogue (deleted
              custom file) — keep them visible so saving doesn't drop them. */}
          {config.profiles
            .filter((n) => !(config.available_profiles ?? []).some((p) => p.name === n))
            .map((n) => (
              <label key={n} className="flex cursor-pointer items-center gap-2.5 rounded-lg bg-stone-500/5 p-2.5 dark:bg-white/5">
                <input
                  type="checkbox"
                  checked
                  onChange={() => setConfig({ ...config, profiles: config.profiles.filter((x) => x !== n) })}
                  className="h-4 w-4 accent-amber-500"
                />
                <span className="text-sm text-stone-700 dark:text-stone-300">{n}</span>
                <Badge tone="warning">{intl.formatMessage({ id: 'redaction.profile.missing' })}</Badge>
              </label>
            ))}
        </div>
        {config.enabled && config.profiles.length === 0 && (
          <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">{intl.formatMessage({ id: 'redaction.profiles.noneSelected' })}</p>
        )}
      </div>

      {/* Sources — mode + per-field scope per row */}
      <div className="border-t border-[var(--panel-border)] pt-4">
        <h4 className="mb-1 text-xs font-semibold uppercase text-stone-500 dark:text-stone-400">{intl.formatMessage({ id: 'redaction.sources' })}</h4>
        <p className="mb-3 text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'redaction.sources.hint' })}</p>
        <div className="space-y-3">
          {REDACTION_SOURCE_KEYS.map((key) => (
            <SourceSettingRow
              key={key}
              sourceKey={key}
              setting={config.sources[key]}
              categories={selectedCategories}
              onChange={(next) => setConfig({ ...config, sources: { ...config.sources, [key]: next } })}
            />
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
          </div>
        )}
      </div>

      <div className="flex items-center justify-end gap-2 pt-2">
        {saved && <span className="text-xs text-emerald-600 dark:text-emerald-400">{intl.formatMessage({ id: 'redaction.savedLive' })}</span>}
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
