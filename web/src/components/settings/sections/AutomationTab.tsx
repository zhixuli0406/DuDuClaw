import { useCallback, useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { useRestartRequiredStore } from '@/stores/restart-required-store';
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
import { Input, SettingsRow } from '@/components/mds';
import { changedPayload, parseTomlSubset, tomlBool, tomlStr, type FlatValues } from '@/lib/config-toml';
import { TakeoverMailSection } from './TakeoverMailSection';
import { TickSettingsSection } from './TickSettingsSection';
import { TeamDefaultsSection } from './TeamDefaultsSection';

// dispatch.policy enum accepted by the gateway's system.update_config.
// v1.68: `role_team` (one employee in four roles) is accepted by the reader
// and, from v1.68, by the RPC as well.
const DISPATCH_POLICIES = ['fixed_hierarchy', 'round_robin', 'llm_select', 'role_team'] as const;

// `[dispatch] judge_provider` — a runtime id (`RuntimeType::parse`); empty =
// no override, the judge runs on the default utility model.
const JUDGE_PROVIDERS = ['', 'claude', 'codex', 'antigravity', 'grok', 'openai_compat'] as const;

// goal_loop.resume_on_restart enum accepted by the gateway's
// system.update_config — kept in "recommended first" order (pause is the
// WP-E default) so the select's natural order matches the safer choice.
const RESUME_ON_RESTART_OPTIONS = ['pause', 'auto'] as const;

// dispatch.judge enum accepted by the gateway's system.update_config (WP-5D
// seam, crates/duduclaw-gateway/src/judge_mode.rs). NOTE: `judge_command` /
// `judge_timeout_secs` are deliberately NOT here and never sent to the RPC —
// they name an executable and stay file-only (config.toml, operator-edited).
// The gateway refuses any other value on write; this list must stay in sync
// with `WRITABLE_JUDGE_MODES` in judge_mode.rs.
const JUDGE_MODES = ['mav', 'external'] as const;

// Removed in v1.69.0. The gateway no longer accepts them on write, but a
// config.toml that still holds one keeps being read: `evaluator_only` runs as
// `mav`, `human_only` parks every review for a person. The picker shows such
// a saved value as it is (labelled with what actually happens) instead of
// displaying `mav`, and never writes it back.
type RemovedJudgeMode = 'evaluator_only' | 'human_only';

/** Map a stored `[dispatch] judge` string to the removed mode it means, using
 *  the same exact-token rules as `JudgeMode::from_config_str` (trim,
 *  ASCII-lowercase, old aliases included). `null` for anything else. */
function removedJudgeMode(raw: string): RemovedJudgeMode | null {
  switch (raw.trim().toLowerCase()) {
    case 'evaluator_only':
    case 'evaluator':
      return 'evaluator_only';
    case 'human_only':
    case 'human':
      return 'human_only';
    default:
      return null;
  }
}

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
  // resume_on_restart defaults to "pause" gateway-side since WP-E (2026-08).
  // Unlike its siblings above, this is a boot-only-read field — it is
  // intentionally NOT part of the hot-reload story described by
  // `hotReloadHint` below, so it carries its own inline restart note.
  const [resumeOnRestart, setResumeOnRestart] = useState('pause');
  // [dispatch] — enabled defaults ON since v1.59 (gateway-side default flip)
  const [dispatchEnabled, setDispatchEnabled] = useState(true);
  const [dispatchPolicy, setDispatchPolicy] = useState('fixed_hierarchy');
  // [dispatch] judge — who adjudicates "is this task actually done" (WP-6B).
  const [judgeMode, setJudgeMode] = useState('mav');
  // v1.68: run the judge on a different (runtime, model) than the worker.
  const [judgeProvider, setJudgeProvider] = useState('');
  const [judgeModel, setJudgeModel] = useState('');
  // v1.68: `[night] llm_enabled` — the model-backed stage of night consolidation.
  const [nightLlm, setNightLlm] = useState(false);
  // What was loaded, flattened by TOML path — saves send only what changed.
  const initialRef = useRef<FlatValues>({});
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
      setResumeOnRestart(strIn(gl, 'resume_on_restart', 'pause'));
      const dp = tomlSection(raw, 'dispatch');
      setDispatchEnabled(boolIn(dp, 'enabled', true));
      setDispatchPolicy(strIn(dp, 'policy', 'fixed_hierarchy'));
      setJudgeMode(strIn(dp, 'judge', 'mav'));
      setTopologyEnabled(boolIn(tomlSection(raw, 'topology_evolution'), 'enabled', false));
      const kg = tomlSection(raw, 'knowledge_guard');
      setKgEnabled(boolIn(kg, 'enabled', true));
      setKgWindowSecs(intIn(kg, 'window_secs', 3600));
      setKgMaxPerSubject(intIn(kg, 'max_per_subject', 5));
      setGraphEmbedSeed(boolIn(tomlSection(raw, 'memory'), 'graph_embed_seed', false));
      setFlatBandPct(floatIn(tomlSection(raw, 'belief'), 'flat_band_pct', 0.3));
      setTickMapText(tickSubjectMapToLines(raw));
      setTickMapError(null);
      const tables = parseTomlSubset(raw);
      const jp = tomlStr(tables, 'dispatch', 'judge_provider', '');
      const jm = tomlStr(tables, 'dispatch', 'judge_model', '');
      const nl = tomlBool(tables, 'night', 'llm_enabled', false);
      setJudgeProvider(jp);
      setJudgeModel(jm);
      setNightLlm(nl);
      const parsedMap = parseTickMapText(tickSubjectMapToLines(raw));
      initialRef.current = snapshotOf({
        plannerEnabled: boolIn(gl, 'planner_enabled', false),
        iterationCapSimple: intIn(gl, 'iteration_cap_simple', 3),
        resumeOnRestart: strIn(gl, 'resume_on_restart', 'pause'),
        dispatchEnabled: boolIn(dp, 'enabled', true),
        dispatchPolicy: strIn(dp, 'policy', 'fixed_hierarchy'),
        judgeMode: strIn(dp, 'judge', 'mav'),
        judgeProvider: jp,
        judgeModel: jm,
        topologyEnabled: boolIn(tomlSection(raw, 'topology_evolution'), 'enabled', false),
        kgEnabled: boolIn(kg, 'enabled', true),
        kgWindowSecs: intIn(kg, 'window_secs', 3600),
        kgMaxPerSubject: intIn(kg, 'max_per_subject', 5),
        graphEmbedSeed: boolIn(tomlSection(raw, 'memory'), 'graph_embed_seed', false),
        flatBandPct: floatIn(tomlSection(raw, 'belief'), 'flat_band_pct', 0.3),
        tickMap: 'map' in parsedMap ? parsedMap.map : {},
        nightLlm: nl,
      });
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
      // v1.68 (plan principle 3): send only the fields that changed, so a
      // save never rewrites a stored value with what the form happened to
      // show. Nothing changed ⇒ no call (the RPC rejects an empty payload).
      const current = snapshotOf({
        plannerEnabled, iterationCapSimple, resumeOnRestart, dispatchEnabled,
        dispatchPolicy, judgeMode, judgeProvider, judgeModel: judgeModel.trim(),
        topologyEnabled, kgEnabled, kgWindowSecs, kgMaxPerSubject, graphEmbedSeed,
        flatBandPct, tickMap: parsed.map, nightLlm,
      });
      const payload = changedPayload(initialRef.current, current);
      if (Object.keys(payload).length === 0) {
        toast.info(intl.formatMessage({ id: 'settings.noChanges' }));
        return;
      }
      const res = await api.system.updateConfig(payload);
      initialRef.current = current;
      const restart = res.restart_required ?? [];
      if (restart.length > 0) useRestartRequiredStore.getState().add(restart);
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
  const resumeOnRestartOptions: SelectOption[] = RESUME_ON_RESTART_OPTIONS.map((v) => ({
    value: v, label: intl.formatMessage({ id: `settings.automation.resumeOnRestart.${v}` }), raw: v,
  }));
  const removedJudge = removedJudgeMode(judgeMode);
  const judgeModeOptions: SelectOption[] = [
    ...JUDGE_MODES.map((v) => ({
      value: v as string,
      label: intl.formatMessage({ id: `settings.automation.judgeMode.${v}` }),
      raw: v as string,
    })),
    // Keep a saved removed value visible (labelled with what actually runs)
    // instead of letting the select show a mode that is not on disk. It is
    // only listed while it is the current value, so it cannot be picked anew.
    ...(removedJudge
      ? [{
          value: judgeMode,
          label: intl.formatMessage({ id: `settings.automation.judgeMode.${removedJudge}` }),
          raw: judgeMode,
        }]
      : []),
  ];

  const judgeProviderOptions: SelectOption[] = [
    ...JUDGE_PROVIDERS.map((v) => ({
      value: v as string,
      label: v === '' ? t('settings.automation.judgeProvider.default') : t(`settings.automation.judgeProvider.${v}`),
      raw: v as string,
    })),
    ...(judgeProvider && !(JUDGE_PROVIDERS as readonly string[]).includes(judgeProvider)
      ? [{ value: judgeProvider, label: judgeProvider, raw: judgeProvider }]
      : []),
  ];

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
          {/* dispatch.judge (WP-6B) — who adjudicates "is this task actually
              done". Re-read on every acceptance decision, so this is hot: no
              restart note needed here (unlike resumeOnRestart below). */}
          <RowSelect
            label={t('settings.automation.judgeMode')}
            description={t('settings.automation.judgeMode.help')}
            value={judgeMode}
            onChange={setJudgeMode}
            options={judgeModeOptions}
          />
          {/* v1.68: judge on a different (runtime, model) than the worker —
              read on every adjudication, so hot. */}
          <RowSelect
            label={t('settings.automation.judgeProvider')}
            description={t('settings.automation.judgeProvider.help')}
            value={judgeProvider}
            onChange={setJudgeProvider}
            options={judgeProviderOptions}
          />
          <SettingsRow
            label={t('settings.automation.judgeModel')}
            description={t('settings.automation.judgeModel.help')}
            tier="text"
          >
            <Input
              type="text"
              value={judgeModel}
              onChange={(e) => setJudgeModel(e.target.value)}
              placeholder={t('settings.automation.judgeModel.placeholder')}
              aria-label={t('settings.automation.judgeModel')}
            />
          </SettingsRow>
          <RowSwitch
            label={t('settings.automation.topologyEnabled')}
            description={t('settings.automation.topologyEnabled.help')}
            checked={topologyEnabled}
            onChange={setTopologyEnabled}
          />
          {/* resume_on_restart is boot-only-read (gateway process restart,
              not the abort+respawn hot reload the other rows in this card
              get) — it gets its own restart note instead of relying on the
              shared hotReloadHint paragraph below, which does not apply to it. */}
          <RowSelect
            label={t('settings.automation.resumeOnRestart')}
            description={t('settings.automation.resumeOnRestart.help')}
            value={resumeOnRestart}
            onChange={setResumeOnRestart}
            options={resumeOnRestartOptions}
          />
        </SettingsCard>
        {/* A saved value removed in v1.69.0: say what is happening now and
            what to pick instead, in the amber tone used for risk callouts. */}
        {removedJudge && (
          <div
            role="status"
            className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2.5 text-xs text-amber-700 dark:text-amber-300"
          >
            {t(`settings.automation.judgeMode.removedNotice.${removedJudge}`)}
          </div>
        )}
        {/* judge_command names an executable and is deliberately NOT settable
            via system.update_config (file-only, operator-edited config.toml)
            — tell the operator where to go instead of leaving an unexplained
            gap where an input field would otherwise be. */}
        {judgeMode === 'external' && (
          <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
            {t('settings.automation.judgeMode.externalHint')}
          </p>
        )}
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
          {/* v1.68: `[night] llm_enabled` — off by default; the zero-cost
              stages of the night pass run either way. */}
          <RowSwitch
            label={t('settings.automation.nightLlm')}
            description={t('settings.automation.nightLlm.help')}
            checked={nightLlm}
            onChange={setNightLlm}
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

      {/* v1.68 (W2) — cards with their own save: each writes only its own
          keys, so saving one never touches another card's settings. */}
      <TakeoverMailSection />
      <TeamDefaultsSection />
      <TickSettingsSection />
    </div>
  );
}

interface AutomationSnapshotInput {
  plannerEnabled: boolean;
  iterationCapSimple: number;
  resumeOnRestart: string;
  dispatchEnabled: boolean;
  dispatchPolicy: string;
  judgeMode: string;
  judgeProvider: string;
  judgeModel: string;
  topologyEnabled: boolean;
  kgEnabled: boolean;
  kgWindowSecs: number;
  kgMaxPerSubject: number;
  graphEmbedSeed: boolean;
  flatBandPct: number;
  tickMap: Record<string, string>;
  nightLlm: boolean;
}

/** Flatten the form into `{ "<toml.path>": value }` — the RPC's nested
 *  param names are the TOML paths (`system.update_config`). */
function snapshotOf(v: AutomationSnapshotInput): FlatValues {
  return {
    'goal_loop.planner_enabled': v.plannerEnabled,
    'goal_loop.iteration_cap_simple': v.iterationCapSimple,
    'goal_loop.resume_on_restart': v.resumeOnRestart,
    'dispatch.enabled': v.dispatchEnabled,
    'dispatch.policy': v.dispatchPolicy,
    'dispatch.judge': v.judgeMode,
    'dispatch.judge_provider': v.judgeProvider,
    'dispatch.judge_model': v.judgeModel,
    'topology_evolution.enabled': v.topologyEnabled,
    'knowledge_guard.enabled': v.kgEnabled,
    'knowledge_guard.window_secs': v.kgWindowSecs,
    'knowledge_guard.max_per_subject': v.kgMaxPerSubject,
    'memory.graph_embed_seed': v.graphEmbedSeed,
    'belief.flat_band_pct': v.flatBandPct,
    'belief.tick_subject_map': v.tickMap,
    'night.llm_enabled': v.nightLlm,
  };
}
