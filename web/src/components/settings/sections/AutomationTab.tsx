import { useCallback, useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  ErrorState,
  SettingsSection,
  SettingsCard,
  SettingsSaveState,
  Textarea,
} from '@/components/mds';
import { type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch, RowNumber } from '@/pages/agent-form/form-rows';
import { AutonomyNote } from '@/components/AutonomyNote';

// dispatch.policy enum accepted by the gateway's system.update_config.
const DISPATCH_POLICIES = ['fixed_hierarchy', 'round_robin', 'llm_select'] as const;

/** Extract the body of a top-level TOML `[section]` from the masked config
 *  string (up to the next `[` header or EOF). Section-scoped so a common key
 *  name (e.g. `enabled`) is read from the right table. */
function tomlSection(raw: string, name: string): string {
  const re = new RegExp(`(?:^|\\n)\\[${name}\\]([\\s\\S]*?)(?:\\n\\[|$)`);
  return raw.match(re)?.[1] ?? '';
}
function boolIn(slice: string, key: string, fallback: boolean): boolean {
  const m = slice.match(new RegExp(`(?:^|\\n)\\s*${key}\\s*=\\s*(true|false)`));
  return m ? m[1] === 'true' : fallback;
}
function intIn(slice: string, key: string, fallback: number): number {
  const m = slice.match(new RegExp(`(?:^|\\n)\\s*${key}\\s*=\\s*(\\d+)`));
  return m ? Number(m[1]) : fallback;
}
function strIn(slice: string, key: string, fallback: string): string {
  const m = slice.match(new RegExp(`(?:^|\\n)\\s*${key}\\s*=\\s*"([^"]*)"`));
  return m ? m[1] : fallback;
}
function floatIn(slice: string, key: string, fallback: number): number {
  const m = slice.match(new RegExp(`(?:^|\\n)\\s*${key}\\s*=\\s*(-?\\d+(?:\\.\\d+)?)`));
  return m ? Number(m[1]) : fallback;
}

/** Extract a nested TOML table's body (e.g. `[belief.tick_subject_map]`) from
 *  the masked config string — `tomlSection` only anchors on a single-segment
 *  header, so a dotted path needs its own regex against the literal header. */
function tomlDottedSection(raw: string, path: string): string {
  const escaped = path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(`(?:^|\\n)\\[${escaped}\\]([\\s\\S]*?)(?:\\n\\[|$)`);
  return raw.match(re)?.[1] ?? '';
}

/** Parse a `[belief.tick_subject_map]` TOML body's `key = "value"` lines into
 *  plain `key=value` text lines for the textarea editor. */
function tickSubjectMapToLines(raw: string): string {
  const body = tomlDottedSection(raw, 'belief\\.tick_subject_map');
  const pairs: string[] = [];
  const lineRe = /^\s*([^\s=]+)\s*=\s*"([^"]*)"\s*$/gm;
  let m: RegExpExecArray | null;
  while ((m = lineRe.exec(body)) !== null) {
    pairs.push(`${m[1]}=${m[2]}`);
  }
  return pairs.join('\n');
}

const TICK_MAP_MAX_PAIRS = 32;
const TICK_MAP_MAX_FIELD_CHARS = 64;

/** Parse the textarea's `key=value` lines (one pair per line, blank lines
 *  ignored) into an object, or an error message describing the first
 *  problem found — mirrors the gateway's `belief.tick_subject_map`
 *  validation (`crates/duduclaw-gateway/src/handlers.rs`) so a malformed
 *  entry never reaches `system.update_config` in the first place. */
function parseTickMapText(text: string): { map: Record<string, string> } | { error: string } {
  const map: Record<string, string> = {};
  const lines = text.split('\n').map((l) => l.trim()).filter((l) => l.length > 0);
  if (lines.length > TICK_MAP_MAX_PAIRS) {
    return { error: `tickMapTooMany:${TICK_MAP_MAX_PAIRS}` };
  }
  for (const line of lines) {
    const idx = line.indexOf('=');
    if (idx <= 0 || idx === line.length - 1) {
      return { error: `tickMapBadLine:${line}` };
    }
    const key = line.slice(0, idx).trim();
    const value = line.slice(idx + 1).trim();
    if (!key || key.length > TICK_MAP_MAX_FIELD_CHARS) {
      return { error: `tickMapBadKey:${key}` };
    }
    if (!value || value.length > TICK_MAP_MAX_FIELD_CHARS) {
      return { error: `tickMapBadValue:${value}` };
    }
    map[key] = value;
  }
  return { map };
}

// ── Automation tab (v1.39 goal-loop / dispatch / topology / knowledge / memory) ──
//
// Groups the five v1.39 config sections into two cards:
//   • "Goal Loop & Dispatch" — the "hard" trio whose long-lived drivers are
//     abort+respawned server-side on save (goal_loop.iteration_cap_simple,
//     dispatch.policy, topology_evolution.enabled) + goal_loop.planner_enabled.
//   • "Knowledge & Memory" — the "easy" per-use-read knobs (knowledge_guard.*,
//     memory.graph_embed_seed) that take effect on the next read.
// There is no dedicated memory/knowledge tab in the shell, so both live here as
// distinct sections (see the UI-placement decision in the change report).
export function AutomationTab() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  // [goal_loop]
  const [plannerEnabled, setPlannerEnabled] = useState(false);
  const [iterationCapSimple, setIterationCapSimple] = useState(3);
  // [dispatch] — enabled defaults ON since v1.59 (gateway-side default flip)
  const [dispatchEnabled, setDispatchEnabled] = useState(true);
  const [dispatchPolicy, setDispatchPolicy] = useState('fixed_hierarchy');
  // [topology_evolution]
  const [topologyEnabled, setTopologyEnabled] = useState(false);
  // [knowledge_guard]
  const [kgEnabled, setKgEnabled] = useState(true);
  const [kgWindowSecs, setKgWindowSecs] = useState(3600);
  const [kgMaxPerSubject, setKgMaxPerSubject] = useState(5);
  // [memory]
  const [graphEmbedSeed, setGraphEmbedSeed] = useState(false);
  // [belief]
  const [flatBandPct, setFlatBandPct] = useState(0.3);
  const [tickMapText, setTickMapText] = useState('');
  const [tickMapError, setTickMapError] = useState<string | null>(null);

  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // P05: on a failed read this form used to render its compiled-in defaults,
  // which reads as "the gateway is set this way" — the most dangerous kind of
  // silent failure on a settings page. Keep the failure visible and retryable.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(() => {
    setLoadError(null);
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw !== 'string') return;
      const gl = tomlSection(raw, 'goal_loop');
      setPlannerEnabled(boolIn(gl, 'planner_enabled', false));
      setIterationCapSimple(intIn(gl, 'iteration_cap_simple', 3));
      const dp = tomlSection(raw, 'dispatch');
      setDispatchEnabled(boolIn(dp, 'enabled', true));
      setDispatchPolicy(strIn(dp, 'policy', 'fixed_hierarchy'));
      setTopologyEnabled(boolIn(tomlSection(raw, 'topology_evolution'), 'enabled', false));
      const kg = tomlSection(raw, 'knowledge_guard');
      setKgEnabled(boolIn(kg, 'enabled', true));
      setKgWindowSecs(intIn(kg, 'window_secs', 3600));
      setKgMaxPerSubject(intIn(kg, 'max_per_subject', 5));
      setGraphEmbedSeed(boolIn(tomlSection(raw, 'memory'), 'graph_embed_seed', false));
      setFlatBandPct(floatIn(tomlSection(raw, 'belief'), 'flat_band_pct', 0.3));
      setTickMapText(tickSubjectMapToLines(raw));
      setTickMapError(null);
    }).catch((e) => {
      console.warn('[api]', e);
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleSave = async () => {
    // Parse the tick_subject_map textarea first — a format error must block
    // the whole save (all automation fields share one updateConfig payload)
    // rather than silently dropping the malformed pairs.
    const parsed = parseTickMapText(tickMapText);
    if ('error' in parsed) {
      // Split on the FIRST colon only — `detail` (a line/key/value from the
      // user's input) may itself contain colons.
      const colonIdx = parsed.error.indexOf(':');
      const kind = colonIdx === -1 ? parsed.error : parsed.error.slice(0, colonIdx);
      const detail = colonIdx === -1 ? '' : parsed.error.slice(colonIdx + 1);
      const msgId = `settings.automation.tickSubjectMap.error.${kind}`;
      const msg = intl.formatMessage({ id: msgId }, { detail, max: TICK_MAP_MAX_PAIRS });
      setTickMapError(msg);
      toast.error(msg);
      return;
    }
    setTickMapError(null);

    setSaving(true);
    setSaved(false);
    setSaveError(null);
    try {
      const res = await api.system.updateConfig({
        goal_loop: { planner_enabled: plannerEnabled, iteration_cap_simple: iterationCapSimple },
        dispatch: { enabled: dispatchEnabled, policy: dispatchPolicy },
        topology_evolution: { enabled: topologyEnabled },
        knowledge_guard: { enabled: kgEnabled, window_secs: kgWindowSecs, max_per_subject: kgMaxPerSubject },
        memory: { graph_embed_seed: graphEmbedSeed },
        belief: { flat_band_pct: flatBandPct, tick_subject_map: parsed.map },
      });
      // Surface which long-lived drivers were hot-reloaded (abort+respawn).
      const hot = res.hot_reloaded ?? [];
      if (hot.length > 0) {
        toast.success(intl.formatMessage({ id: 'settings.automation.hotReloaded' }, { drivers: hot.join(', ') }));
      }
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.warn('[api]', e);
      setSaveError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const policyOptions: SelectOption[] = DISPATCH_POLICIES.map((v) => ({
    value: v, label: intl.formatMessage({ id: `settings.automation.policy.${v}` }), raw: v,
  }));

  return (
    <div className="space-y-8">
      {loadError != null && (
        <ErrorState
          variant="inline"
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          onRetry={load}
        />
      )}

      {/* Goal loop / dispatch / topology — "hard" trio, hot-reloaded on save */}
      <SettingsSection
        title={t('settings.automation.goalLoop')}
        description={t('settings.automation.goalLoop.desc')}
      >
        <SettingsCard>
          <RowSwitch
            label={t('settings.automation.dispatchEnabled')}
            description={t('settings.automation.dispatchEnabled.help')}
            checked={dispatchEnabled}
            onChange={setDispatchEnabled}
          />
          <RowSwitch
            label={t('settings.automation.plannerEnabled')}
            description={t('settings.automation.plannerEnabled.help')}
            checked={plannerEnabled}
            onChange={setPlannerEnabled}
          />
          <RowNumber
            label={t('settings.automation.iterationCapSimple')}
            description={t('settings.automation.iterationCapSimple.help')}
            value={iterationCapSimple}
            min={1}
            max={20}
            onChange={setIterationCapSimple}
          />
          <RowSelect
            label={t('settings.automation.dispatchPolicy')}
            description={t('settings.automation.dispatchPolicy.help')}
            value={dispatchPolicy}
            onChange={setDispatchPolicy}
            options={policyOptions}
          />
          <RowSwitch
            label={t('settings.automation.topologyEnabled')}
            description={t('settings.automation.topologyEnabled.help')}
            checked={topologyEnabled}
            onChange={setTopologyEnabled}
          />
        </SettingsCard>
        <AutonomyNote id="automation" />
        <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
          {t('settings.automation.hotReloadHint')}
        </p>
      </SettingsSection>

      {/* Knowledge guard + memory seed — "easy" per-use-read knobs */}
      <SettingsSection
        title={t('settings.automation.knowledge')}
        description={t('settings.automation.knowledge.desc')}
      >
        <SettingsCard>
          <RowSwitch
            label={t('settings.automation.kgEnabled')}
            description={t('settings.automation.kgEnabled.help')}
            checked={kgEnabled}
            onChange={setKgEnabled}
          />
          <RowNumber
            label={t('settings.automation.kgWindowSecs')}
            description={t('settings.automation.kgWindowSecs.help')}
            value={kgWindowSecs}
            min={1}
            max={604800}
            onChange={setKgWindowSecs}
          />
          <RowNumber
            label={t('settings.automation.kgMaxPerSubject')}
            description={t('settings.automation.kgMaxPerSubject.help')}
            value={kgMaxPerSubject}
            min={1}
            max={10000}
            onChange={setKgMaxPerSubject}
          />
          <RowSwitch
            label={t('settings.automation.graphEmbedSeed')}
            description={t('settings.automation.graphEmbedSeed.help')}
            checked={graphEmbedSeed}
            onChange={setGraphEmbedSeed}
          />
        </SettingsCard>
        <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
          {t('settings.automation.appliedHint')}
        </p>
      </SettingsSection>

      {/* Belief loop — flat-band tolerance + tick-field↔subject mapping,
          both "easy" per-use-read knobs. */}
      <SettingsSection
        title={t('settings.automation.belief')}
        description={t('settings.automation.belief.desc')}
      >
        <SettingsCard>
          <RowNumber
            label={t('settings.automation.flatBandPct')}
            description={t('settings.automation.flatBandPct.help')}
            value={flatBandPct}
            min={0.01}
            max={10}
            step={0.01}
            onChange={setFlatBandPct}
          />
        </SettingsCard>
        <div className="space-y-1.5">
          <div className="text-sm font-medium text-foreground">
            {t('settings.automation.tickSubjectMap')}
          </div>
          <p className="text-xs text-muted-foreground">
            {t('settings.automation.tickSubjectMap.help')}
          </p>
          <Textarea
            value={tickMapText}
            onChange={(e) => {
              setTickMapText(e.target.value);
              setTickMapError(null);
            }}
            rows={4}
            spellCheck={false}
            placeholder={t('settings.automation.tickSubjectMap.placeholder')}
            className="font-mono text-xs"
          />
          {tickMapError != null && (
            <p className="text-xs text-destructive">{tickMapError}</p>
          )}
        </div>
        <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
          {t('settings.automation.appliedHint')}
        </p>
      </SettingsSection>

      {saveError != null && (
        <ErrorState
          variant="inline"
          error={saveError}
          title={intl.formatMessage({ id: 'errorState.manage.saveFailed' })}
          description={intl.formatMessage({ id: 'errorState.manage.saveFailedHint' })}
          onRetry={() => void handleSave()}
        />
      )}
      <div className="flex items-center justify-end gap-3">
        <SettingsSaveState
          status={saving ? 'saving' : saved ? 'saved' : 'idle'}
          savingLabel={intl.formatMessage({ id: 'common.saving' })}
          savedLabel={intl.formatMessage({ id: 'settings.general.saved' })}
        />
        <Button variant="brand" size="sm" onClick={handleSave} disabled={saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </div>
  );
}
