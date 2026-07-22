import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  SettingsSection,
  SettingsCard,
  SettingsSaveState,
} from '@/components/mds';
import { type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch, RowNumber } from '@/pages/agent-form/form-rows';

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
  // [dispatch]
  const [dispatchPolicy, setDispatchPolicy] = useState('fixed_hierarchy');
  // [topology_evolution]
  const [topologyEnabled, setTopologyEnabled] = useState(false);
  // [knowledge_guard]
  const [kgEnabled, setKgEnabled] = useState(true);
  const [kgWindowSecs, setKgWindowSecs] = useState(3600);
  const [kgMaxPerSubject, setKgMaxPerSubject] = useState(5);
  // [memory]
  const [graphEmbedSeed, setGraphEmbedSeed] = useState(false);

  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  useEffect(() => {
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw !== 'string') return;
      const gl = tomlSection(raw, 'goal_loop');
      setPlannerEnabled(boolIn(gl, 'planner_enabled', false));
      setIterationCapSimple(intIn(gl, 'iteration_cap_simple', 3));
      setDispatchPolicy(strIn(tomlSection(raw, 'dispatch'), 'policy', 'fixed_hierarchy'));
      setTopologyEnabled(boolIn(tomlSection(raw, 'topology_evolution'), 'enabled', false));
      const kg = tomlSection(raw, 'knowledge_guard');
      setKgEnabled(boolIn(kg, 'enabled', true));
      setKgWindowSecs(intIn(kg, 'window_secs', 3600));
      setKgMaxPerSubject(intIn(kg, 'max_per_subject', 5));
      setGraphEmbedSeed(boolIn(tomlSection(raw, 'memory'), 'graph_embed_seed', false));
    }).catch((e) => {
      console.warn('[api]', e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    try {
      const res = await api.system.updateConfig({
        goal_loop: { planner_enabled: plannerEnabled, iteration_cap_simple: iterationCapSimple },
        dispatch: { policy: dispatchPolicy },
        topology_evolution: { enabled: topologyEnabled },
        knowledge_guard: { enabled: kgEnabled, window_secs: kgWindowSecs, max_per_subject: kgMaxPerSubject },
        memory: { graph_embed_seed: graphEmbedSeed },
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
      {/* Goal loop / dispatch / topology — "hard" trio, hot-reloaded on save */}
      <SettingsSection
        title={t('settings.automation.goalLoop')}
        description={t('settings.automation.goalLoop.desc')}
      >
        <SettingsCard>
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
