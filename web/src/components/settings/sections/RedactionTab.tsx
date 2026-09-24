import { useEffect, useState, useCallback, useRef, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import {
  api,
  type RedactionSourceMode,
  type RedactionSourceSetting,
  type RedactionSources,
  type RedactionStats,
  type RedactionPolicyStatus,
  type RedactionAuditEntry,
} from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Card,
  CardHeader,
  CardTitle,
  CardContent,
  Button,
  Badge,
  ErrorState,
  Input,
  Switch,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/mds';
import { FieldBlock } from '@/pages/agent-form/form-rows';
import { ConfirmDialog } from '@/components/settings/controls';
import { RedactionFieldRulesCard } from './RedactionFieldRulesCard';
import { RedactionSystemsCard } from './RedactionSystemsCard';
import { RedactionMyRulesCard } from './RedactionMyRulesCard';
import { RedactionAiDetectionRow } from './RedactionAiDetectionRow';
import {
  DATA_FILE_GUARD_MODES,
  type DataFileGuardMode,
  type RedactionConfigWithGuard,
  type RedactionUpdateWithGuard,
} from './redactionFieldRules';
import {
  ScrollText,
  ShieldCheck,
  RefreshCw,
  AlertTriangle,
  ChevronDown,
  Info,
} from 'lucide-react';

// ── Privacy / Redaction Tab (RED) ──────────────────────────────
//
// This tab is written for a non-engineer operator. The redaction ENGINE speaks
// in `Source × Mode` matrices and `tool_egress` globs; here we translate that
// into plain "what data gets protected" + "which external systems" language, and
// tuck the raw knobs (vault TTL, purge, profiles) behind an "Advanced" fold.

// Semantic tone → mds Badge className (the mds Badge only ships neutral variants).
const TONE_CLASS: Record<'success' | 'warning' | 'danger' | 'neutral', string> = {
  success: 'bg-success/10 text-success',
  warning: 'bg-warning/10 text-warning',
  danger: 'bg-destructive/10 text-destructive',
  neutral: 'bg-muted text-muted-foreground',
};

export function ToneBadge({
  tone,
  dot,
  children,
}: {
  tone: 'success' | 'warning' | 'danger' | 'neutral';
  dot?: boolean;
  children: ReactNode;
}) {
  return (
    <Badge className={TONE_CLASS[tone]}>
      {dot && <span className="size-1.5 rounded-full bg-current" />}
      {children}
    </Badge>
  );
}

/** Small inline enum dropdown (mds Select) for the source-mode / egress pickers. */
function EnumSelect({
  value,
  onChange,
  options,
  className,
  ariaLabel,
}: {
  value: string;
  onChange: (v: string) => void;
  options: ReadonlyArray<{ value: string; label: ReactNode }>;
  className?: string;
  ariaLabel?: string;
}) {
  const current = options.find((o) => o.value === value);
  return (
    <Select value={value} onValueChange={(v) => onChange(String(v))}>
      <SelectTrigger size="sm" className={className} aria-label={ariaLabel}>
        <SelectValue>{current?.label}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {options.map((o) => (
          <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

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

/** Human label for a PII category. DESIGN-redaction-ner-and-custom-rules
 *  §13.2: a user-typed "我的規則" / imported-profile category (`CUSTOM_*`)
 *  has no `redaction.cat.*` i18n entry — its display name instead lives in
 *  `RedactionConfig.category_labels` (merged from every loaded profile's
 *  `[meta.labels]`), which this checks FIRST. `labels` is optional and
 *  defaults to "none" so every pre-existing call site keeps working
 *  byte-for-byte without passing it. */
export function categoryLabel(
  intl: ReturnType<typeof useIntl>,
  cat: string,
  labels?: Record<string, string>,
): string {
  const custom = labels?.[cat];
  if (custom) return custom;
  return intl.formatMessage({ id: `redaction.cat.${cat}`, defaultMessage: cat });
}

/** Which category-filter shape a source setting is currently in. */
type FieldScope = 'all' | 'only' | 'exclude';
function scopeOf(s: RedactionSourceSetting): FieldScope {
  if (s.only_categories.length > 0) return 'only';
  if (s.exclude_categories.length > 0) return 'exclude';
  return 'all';
}

/** One source row: mode select + expandable per-category scope editor. */
function SourceSettingRow({
  sourceKey,
  setting,
  categories,
  categoryLabels,
  onChange,
}: {
  sourceKey: keyof RedactionSources;
  setting: RedactionSourceSetting;
  /** Union of categories covered by the currently selected profiles. */
  categories: string[];
  /** `RedactionConfig.category_labels` — passed through to `categoryLabel`. */
  categoryLabels?: Record<string, string>;
  onChange: (next: RedactionSourceSetting) => void;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(false);
  const scope = scopeOf(setting);
  // The category filter only matters when this source actually redacts.
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
          <p className="text-sm text-foreground">
            {intl.formatMessage({ id: `redaction.source.${sourceKey}` })}
          </p>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {intl.formatMessage({ id: `redaction.source.${sourceKey}.desc` })}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {filterable && (
            <button
              onClick={() => setOpen((v) => !v)}
              className="flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-muted-foreground hover:bg-muted"
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
          <EnumSelect
            value={setting.mode}
            onChange={(v) => onChange({ ...setting, mode: v as RedactionSourceMode })}
            options={REDACTION_MODES.map((m) => ({ value: m, label: intl.formatMessage({ id: `redaction.mode.${m}` }) }))}
            className="w-32"
            ariaLabel={intl.formatMessage({ id: `redaction.source.${sourceKey}` })}
          />
        </div>
      </div>

      {filterable && open && (
        <div className="mt-2 rounded-lg bg-muted/50 p-3">
          <div className="mb-2 flex flex-wrap gap-3">
            {(['all', 'only', 'exclude'] as const).map((s) => (
              <label key={s} className="flex items-center gap-1.5 text-xs text-foreground">
                <input
                  type="radio"
                  name={`fields-${sourceKey}`}
                  checked={scope === s}
                  onChange={() => setScope(s)}
                  className="accent-primary"
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
                          ? 'border-brand/40 bg-brand/10 text-brand'
                          : 'border-input text-muted-foreground hover:bg-muted',
                      )}
                      aria-pressed={checked}
                    >
                      {categoryLabel(intl, cat, categoryLabels)}
                    </button>
                  );
                })}
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'redaction.fields.noneAvailable' })}
              </p>
            )
          )}
          {scope !== 'all' && activeList.length === 0 && (
            <p className="mt-2 text-xs text-warning">
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
  const [config, setConfig] = useState<RedactionConfigWithGuard | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // P05: the audit section below already had a proper inline error card; the
  // main form did not, so a failed read left it on "載入中…" indefinitely.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  // §13.2 `redaction.profiles.remove` — removing an imported rule pack (or
  // the "我的規則" profile itself) from the 偵測規則集 list above.
  const [removeProfileTarget, setRemoveProfileTarget] = useState<string | null>(null);
  const [removingProfile, setRemovingProfile] = useState(false);
  const confirmRemoveProfile = async () => {
    if (!removeProfileTarget) return;
    setRemovingProfile(true);
    try {
      const result = await api.redaction.profiles.remove(removeProfileTarget);
      setRemoveProfileTarget(null);
      await load();
      // Deleted on disk but the reload after removal failed to apply — say
      // so rather than a plain success toast.
      if (result.warning) toast.error(result.warning);
      else toast.success(intl.formatMessage({ id: 'redaction.savedLive' }));
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setRemovingProfile(false);
    }
  };

  // Which system the "資料表欄位規則" card below is currently filtered to —
  // set by the merged systems card's "N 條欄位規則" chip (§15.1). `fieldRulesRef`
  // is what that chip scrolls into view.
  const [ruleSourceFilter, setRuleSourceFilter] = useState<string | null>(null);
  const [ruleSourceFilterLabel, setRuleSourceFilterLabel] = useState<string | null>(null);
  const fieldRulesRef = useRef<HTMLDivElement>(null);
  const jumpToRules = useCallback((source: string, label: string) => {
    setRuleSourceFilter(source);
    setRuleSourceFilterLabel(label);
    fieldRulesRef.current?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }, []);

  // Returns the fresh config so callers (the field-rules / systems cards,
  // which save independently of this tab's own big batched Save button) can
  // read an accurate post-save count without racing this component's own
  // re-render.
  const load = useCallback(async (): Promise<RedactionConfigWithGuard | null> => {
    setLoadError(null);
    try {
      const res = await api.redaction.get();
      setConfig(res);
      return res;
    } catch (e) {
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      return null;
    }
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    setSaveError(null);
    try {
      // `tool_egress` is deliberately NOT part of this batched payload: the
      // 外部系統與資料來源 card below saves it immediately, per-row, via its
      // own `redaction.update` calls (matching the field-rules / data-sources
      // cards' pattern) — nothing on this form's own controls touches it.
      const payload: RedactionUpdateWithGuard = {
        enabled: config.enabled,
        vault_ttl_hours: config.vault_ttl_hours,
        purge_after_expire_days: config.purge_after_expire_days,
        profiles: config.profiles,
        sources: config.sources,
        data_file_guard: config.data_file_guard,
      };
      const res = await api.redaction.update(payload);
      if (res.warning) {
        // Saved to disk but NOT live — say so instead of pretending success.
        toast.error(res.warning);
      } else {
        setSaved(true);
        savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
      }
    } catch (e) {
      setSaveError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  if (!config) {
    return loadError != null ? (
      <ErrorState
        error={loadError}
        title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
        onRetry={() => void load()}
      />
    ) : (
      <p className="py-8 text-center text-sm text-muted-foreground">
        {intl.formatMessage({ id: 'common.loading' })}
      </p>
    );
  }

  // Categories the current profile selection can recognise — feeds every source
  // row's per-category scope picker.
  const selectedCategories = Array.from(
    new Set(
      (config.available_profiles ?? [])
        .filter((p) => config.profiles.includes(p.name))
        .flatMap((p) => p.categories),
    ),
  ).sort();

  return (
    <div className="space-y-6">
      <Card>
        <CardContent className="space-y-6">
          {/* Poison banner (canvas screen 7) — fixed at the very top, above the
              master toggle: this is the whole card's precondition, everything
              below is moot until it clears. Destructive tone, not warning —
              this isn't a heads-up, protection genuinely isn't running. */}
          {config.poisoned && (
            <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3.5 py-3 text-sm">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
              <div className="space-y-1">
                <p className="font-medium text-foreground">{intl.formatMessage({ id: 'redaction.poison.title' })}</p>
                <p className="text-xs leading-relaxed text-muted-foreground">
                  <code className="rounded bg-muted px-1 py-0.5 font-mono text-foreground">{config.poisoned.reason}</code>
                </p>
                <p className="font-mono text-xs text-muted-foreground">
                  {intl.formatMessage(
                    { id: 'redaction.poison.since' },
                    { time: new Date(config.poisoned.since).toLocaleString(intl.locale) },
                  )}
                </p>
              </div>
            </div>
          )}

          {/* Plain-language explainer: what this feature actually does. */}
          <div className="flex gap-2.5 rounded-lg bg-warning/5 px-3.5 py-3 text-xs leading-relaxed text-muted-foreground">
            <Info className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
            <span>{intl.formatMessage({ id: 'redaction.intro' })}</span>
          </div>

          {/* Master toggle */}
          <div className="flex items-center justify-between py-1.5">
            <span className="text-sm font-medium text-foreground">{intl.formatMessage({ id: 'redaction.enabled' })}</span>
            <Switch
              checked={config.enabled}
              onCheckedChange={(v) => setConfig({ ...config, enabled: Boolean(v) })}
              aria-label={intl.formatMessage({ id: 'redaction.enabled' })}
            />
          </div>

          {/* Detection rule sets (profiles) — what categories CAN be recognised */}
          <div className="border-t border-surface-border pt-4">
            <h4 className="mb-1 text-xs font-semibold uppercase text-muted-foreground">{intl.formatMessage({ id: 'redaction.profiles.title' })}</h4>
            <p className="mb-3 text-xs text-muted-foreground">{intl.formatMessage({ id: 'redaction.profiles.desc' })}</p>
            <div className="space-y-2">
              {(config.available_profiles ?? []).map((p) => {
                const checked = config.profiles.includes(p.name);
                // §13.4 — the `ai_pii` (NER) profile can't compile until its
                // local model is installed, so it gets the model-status
                // three-state row instead of the plain checkbox+categories
                // row below (canvas screen 1, WP-N2).
                if (p.requires_model) {
                  return (
                    <RedactionAiDetectionRow
                      key={p.name}
                      profile={p}
                      checked={checked}
                      categoryLabels={config.category_labels}
                      onToggle={() =>
                        setConfig({
                          ...config,
                          profiles: checked
                            ? config.profiles.filter((n) => n !== p.name)
                            : [...config.profiles, p.name],
                        })
                      }
                    />
                  );
                }
                return (
                  <label key={p.name} className="flex cursor-pointer items-start gap-2.5 rounded-lg bg-muted/50 p-2.5">
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
                      className="mt-0.5 h-4 w-4 accent-primary"
                    />
                    <span className="min-w-0 flex-1">
                      <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium text-foreground">
                        {/* A custom profile (「我的規則」or an imported pack) has
                            no curated `redaction.profile.<name>` i18n entry —
                            `p.label` (the profile's own `[meta] name`) is its
                            only display name. A BUILT-IN profile still uses the
                            translated i18n string; `p.label` there is the raw,
                            untranslated TOML value, not a substitute. */}
                        {(p.custom ?? !p.builtin)
                          ? p.label
                          : intl.formatMessage({ id: `redaction.profile.${p.name}`, defaultMessage: p.name })}
                        {(p.custom ?? !p.builtin) && <Badge variant="outline">{intl.formatMessage({ id: 'redaction.profile.custom' })}</Badge>}
                      </span>
                      <span className="mt-1 flex flex-wrap gap-1">
                        {p.categories.map((cat) => (
                          <span key={cat} className="rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">
                            {categoryLabel(intl, cat, config.category_labels)}
                          </span>
                        ))}
                      </span>
                    </span>
                    {/* §13.2 `available_profiles[].custom` — an imported rule pack
                        (or the "我的規則" profile itself) can be removed as a
                        whole unit; built-in profiles never render this. */}
                    {(p.custom ?? !p.builtin) && (
                      <Button
                        variant="ghost"
                        size="xs"
                        className="shrink-0 text-destructive hover:bg-destructive/10"
                        onClick={(e) => {
                          e.preventDefault();
                          setRemoveProfileTarget(p.name);
                        }}
                      >
                        {intl.formatMessage({ id: 'redaction.import.removeProfile' })}
                      </Button>
                    )}
                  </label>
                );
              })}
              {/* Config may reference profiles missing from the catalogue (deleted
                  custom file) — keep them visible so saving doesn't drop them. */}
              {config.profiles
                .filter((n) => !(config.available_profiles ?? []).some((p) => p.name === n))
                .map((n) => (
                  <label key={n} className="flex cursor-pointer items-center gap-2.5 rounded-lg bg-muted/50 p-2.5">
                    <input
                      type="checkbox"
                      checked
                      onChange={() => setConfig({ ...config, profiles: config.profiles.filter((x) => x !== n) })}
                      className="h-4 w-4 accent-primary"
                    />
                    <span className="text-sm text-foreground">{n}</span>
                    <ToneBadge tone="warning">{intl.formatMessage({ id: 'redaction.profile.missing' })}</ToneBadge>
                  </label>
                ))}
            </div>
            {config.enabled && config.profiles.length === 0 && (
              <p className="mt-2 text-xs text-warning">{intl.formatMessage({ id: 'redaction.profiles.noneSelected' })}</p>
            )}
          </div>

          <ConfirmDialog
            open={removeProfileTarget !== null}
            onClose={() => setRemoveProfileTarget(null)}
            onConfirm={() => void confirmRemoveProfile()}
            busy={removingProfile}
            title={intl.formatMessage({ id: 'redaction.import.removeProfile.confirm.title' })}
            message={intl.formatMessage(
              { id: 'redaction.import.removeProfile.confirm.message' },
              { name: removeProfileTarget ?? '' },
            )}
          />

          {/* "我的規則" (canvas screens 2-9, §12.2) — directly below the
              detection rule-set list above (§12.2: "放在偵測規則集卡正下
              方"): that section decides WHICH built-in or imported profiles
              are active, this one lets the operator add their own
              keyword/pattern rules without writing TOML. Fetches and saves
              independently of this tab's batched Save button; `onReload`
              re-pulls `redaction.get` so a saved rule's profile ("我的規則")
              shows up ticked in the list above without a page refresh. */}
          <RedactionMyRulesCard
            config={config}
            selectedCategories={selectedCategories}
            onReload={load}
          />

          {/* Sources — mode + per-category scope per row */}
          <div className="border-t border-surface-border pt-4">
            <h4 className="mb-1 text-xs font-semibold uppercase text-muted-foreground">{intl.formatMessage({ id: 'redaction.sources' })}</h4>
            <p className="mb-3 text-xs text-muted-foreground">{intl.formatMessage({ id: 'redaction.sources.hint' })}</p>
            <div className="space-y-3">
              {REDACTION_SOURCE_KEYS.map((key) => (
                <SourceSettingRow
                  key={key}
                  sourceKey={key}
                  setting={config.sources[key]}
                  categories={selectedCategories}
                  categoryLabels={config.category_labels}
                  onChange={(next) => setConfig({ ...config, sources: { ...config.sources, [key]: next } })}
                />
              ))}
            </div>
          </div>

          {/* Merged "外部系統與資料來源" card (canvas screen 9, §15) — replaces
              the old separate "外部系統" tool_egress block AND the standalone
              資料來源 card below it: one row per configured system (Odoo /
              each db_sources connection / each custom data source / the
              fixed "地端檔案" row), "讀進來" + "寫回去" side by side. Saves
              through its own immediate RPC calls, independent of this tab's
              batched Save button below. */}
          <RedactionSystemsCard
            toolEgress={config.tool_egress}
            dataSources={config.data_sources ?? []}
            fieldRules={config.field_rules ?? []}
            onReload={load}
            onJumpToRules={jumpToRules}
          />

          {/* Table field rules (§13, canvas screens 1-6) — directly under the
              merged systems card: that card governs "restore on write-back",
              this one governs "which fields get masked on read". Same data
              sources, two directions, so they sit next to each other. Saves
              through its own immediate RPC calls (field_rules is an
              upsert-merge map), independent of this tab's batched Save
              button below. Wrapped so the systems card's "N 條欄位規則" chip
              has a scroll target. */}
          <div ref={fieldRulesRef}>
            <RedactionFieldRulesCard
              fieldRules={config.field_rules ?? []}
              dataSources={config.data_sources ?? []}
              categoryLabels={config.category_labels}
              onReload={load}
              sourceFilter={ruleSourceFilter}
              sourceFilterLabel={ruleSourceFilterLabel}
              onClearSourceFilter={() => { setRuleSourceFilter(null); setRuleSourceFilterLabel(null); }}
            />
          </div>

          {/* Advanced — the raw retention / profile knobs, folded away by default */}
          <div className="border-t border-surface-border pt-4">
            <button
              onClick={() => setShowAdvanced((v) => !v)}
              className="flex w-full items-center justify-between text-xs font-semibold uppercase text-muted-foreground hover:text-foreground"
            >
              {intl.formatMessage({ id: 'redaction.advanced' })}
              <ChevronDown className={cn('h-4 w-4 transition-transform', showAdvanced && 'rotate-180')} />
            </button>
            {showAdvanced && (
              <div className="mt-4 space-y-4">
                <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'redaction.advanced.hint' })}</p>
                <div className="grid gap-4 sm:grid-cols-2">
                  <FieldBlock label={intl.formatMessage({ id: 'redaction.vaultTtl' })} description="1-8760">
                    <Input type="number" min={1} max={8760} value={config.vault_ttl_hours} onChange={(e) => setConfig({ ...config, vault_ttl_hours: Number(e.target.value) })} />
                  </FieldBlock>
                  <FieldBlock label={intl.formatMessage({ id: 'redaction.purgeAfter' })} description="0-3650">
                    <Input type="number" min={0} max={3650} value={config.purge_after_expire_days} onChange={(e) => setConfig({ ...config, purge_after_expire_days: Number(e.target.value) })} />
                  </FieldBlock>
                </div>

                {/* Data-file guard (§14.4) — whether the built-in Read/Bash
                    tools can be used to route around de-identified
                    csv_read/xlsx_read/file_read. */}
                <FieldBlock label={intl.formatMessage({ id: 'redaction.guard.label' })} description={intl.formatMessage({ id: 'redaction.guard.hint' })}>
                  <Select
                    value={config.data_file_guard ?? 'on'}
                    onValueChange={(v) => setConfig({ ...config, data_file_guard: (v ?? 'on') as DataFileGuardMode })}
                  >
                    <SelectTrigger className="w-full sm:w-72">
                      <SelectValue>{intl.formatMessage({ id: `redaction.guard.mode.${config.data_file_guard ?? 'on'}` })}</SelectValue>
                    </SelectTrigger>
                    <SelectContent>
                      {DATA_FILE_GUARD_MODES.map((m) => (
                        <SelectItem key={m} value={m}>{intl.formatMessage({ id: `redaction.guard.mode.${m}` })}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <p className="mt-1.5 text-xs text-muted-foreground">{intl.formatMessage({ id: 'redaction.guard.note' })}</p>
                </FieldBlock>
              </div>
            )}
          </div>

          {saveError != null && (
            <ErrorState
              variant="inline"
              error={saveError}
              title={intl.formatMessage({ id: 'errorState.manage.saveFailed' })}
              description={intl.formatMessage({ id: 'errorState.manage.saveFailedHint' })}
              onRetry={() => void handleSave()}
            />
          )}

          <div className="flex items-center justify-end gap-2 pt-2">
            {saved && <span className="text-xs text-success">{intl.formatMessage({ id: 'redaction.savedLive' })}</span>}
            <Button variant="brand" size="sm" onClick={handleSave} disabled={saving}>
              {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
            </Button>
          </div>
        </CardContent>
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
    <Card>
      <CardHeader className="grid-cols-[1fr_auto] items-center">
        <CardTitle className="flex items-center gap-2 text-sm">
          <ScrollText className="h-4 w-4 text-warning" />
          {intl.formatMessage({ id: 'redaction.audit.title' })}
        </CardTitle>
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => load(true)}
          disabled={loading || refreshing}
          title={intl.formatMessage({ id: 'common.refresh' })}
          className={cn(refreshing && '[&_svg]:animate-spin')}
        >
          <RefreshCw />
        </Button>
      </CardHeader>
      <CardContent className="space-y-5">
        <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'redaction.audit.desc' })}</p>

        {loading ? (
          <div className="space-y-2">
            {[0, 1, 2].map((i) => (
              <div key={i} className="h-10 animate-pulse rounded-lg bg-muted/50" />
            ))}
          </div>
        ) : error ? (
          <div className="flex items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-sm text-destructive">
            <AlertTriangle className="h-4 w-4 shrink-0" />
            <span>{intl.formatMessage({ id: 'redaction.audit.error' }, { message: error })}</span>
          </div>
        ) : (
          <>
            {/* Policy status pills. A poisoned config takes precedence over
                "on" — this pill and the settings-card banner above must never
                disagree about whether protection is actually running. */}
            <div className="flex flex-wrap items-center gap-2">
              <ToneBadge tone={policy?.poisoned ? 'danger' : enabled ? 'success' : 'neutral'} dot>
                {policy?.poisoned
                  ? intl.formatMessage({ id: 'redaction.poison.policyPill' })
                  : enabled
                    ? intl.formatMessage({ id: 'redaction.audit.policyOn' })
                    : intl.formatMessage({ id: 'redaction.audit.policyOff' })}
              </ToneBadge>
              {policy && (
                <>
                  <ToneBadge tone="neutral">
                    {intl.formatMessage({ id: 'redaction.audit.ruleCount' }, { count: policy.rule_count })}
                  </ToneBadge>
                  <ToneBadge tone="neutral">
                    {intl.formatMessage({ id: 'redaction.audit.ttl' }, { hours: policy.vault_ttl_hours })}
                  </ToneBadge>
                  {policy.override_active && (
                    <ToneBadge tone="warning" dot>
                      {intl.formatMessage({ id: 'redaction.audit.overrideActive' })}
                    </ToneBadge>
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
              <h4 className="mb-2 flex items-center gap-1.5 text-xs font-semibold uppercase text-muted-foreground">
                <ShieldCheck className="h-3.5 w-3.5" />
                {intl.formatMessage({ id: 'redaction.audit.categories' })}
              </h4>
              {stats && stats.vault.by_category.length > 0 ? (
                <div className="flex flex-wrap gap-2">
                  {stats.vault.by_category.map(([cat, count]) => (
                    <span
                      key={cat}
                      className="inline-flex items-center gap-1.5 rounded-full bg-warning/10 px-2.5 py-1 text-xs font-medium text-warning"
                    >
                      {cat}
                      <span className="rounded-full bg-warning/20 px-1.5 tabular-nums">{count}</span>
                    </span>
                  ))}
                </div>
              ) : (
                <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'redaction.audit.noCategories' })}</p>
              )}
            </div>

            {/* Recent audit records */}
            <div>
              <h4 className="mb-2 text-xs font-semibold uppercase text-muted-foreground">
                {intl.formatMessage({ id: 'redaction.audit.recent' })}
              </h4>
              {audit && audit.length > 0 ? (
                <ul className="space-y-1.5">
                  {audit.map((entry, i) => (
                    <li
                      key={i}
                      className="flex flex-wrap items-center gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs"
                    >
                      <ToneBadge tone={auditEventTone(entry.event)}>{auditEventLabel(intl, entry.event)}</ToneBadge>
                      {entry.category && (
                        <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-muted-foreground">{entry.category}</code>
                      )}
                      {entry.tool && (
                        <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-muted-foreground">{entry.tool}</code>
                      )}
                      {entry.agent_id && <span className="text-muted-foreground">{entry.agent_id}</span>}
                      {entry.ts && (
                        <span className="ml-auto font-mono text-muted-foreground">
                          {new Date(entry.ts).toLocaleString('zh-TW', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="rounded-lg bg-muted/50 px-3 py-4 text-center text-xs text-muted-foreground">
                  {intl.formatMessage({ id: 'redaction.audit.noRecords' })}
                </p>
              )}
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

function VaultStat({ label, value, tone = 'amber' }: { label: string; value: number; tone?: 'amber' | 'emerald' | 'stone' }) {
  const color =
    tone === 'emerald'
      ? 'text-success'
      : tone === 'stone'
        ? 'text-muted-foreground'
        : 'text-warning';
  return (
    <div className="rounded-lg bg-muted/50 px-3 py-2.5">
      <p className={cn('text-xl font-bold tabular-nums', color)}>{value.toLocaleString()}</p>
      <p className="mt-0.5 text-xs text-muted-foreground">{label}</p>
    </div>
  );
}
