import { useIntl } from 'react-intl';
import { Button, ErrorState, Input, SettingsCard, SettingsRow, SettingsSection } from '@/components/mds';
import { AdvancedSection, type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch } from '@/pages/agent-form/form-rows';
import { tomlBool, tomlStr, type TomlTables } from '@/lib/config-toml';
import { useConfigForm, vBool, vStr } from '../useConfigForm';

const TEAM_ROLES = ['planner', 'executor', 'verifier', 'synthesizer'] as const;
type TeamRole = (typeof TEAM_ROLES)[number];
const TEAM_GATES = ['auto', 'always_solo', 'always_team'] as const;
// duduclaw_core::types::TEAM_ROLE_RUNTIME_ALLOWLIST minus the deprecated
// `gemini` (a saved value stays visible, labelled, below).
const ROLE_RUNTIMES = ['', 'claude', 'codex', 'antigravity', 'grok'] as const;
const EFFORTS = ['', 'low', 'medium', 'high', 'xhigh', 'max'] as const;

function readRoles(tables: TomlTables): Record<string, string> {
  const out: Record<string, string> = {};
  for (const role of TEAM_ROLES) {
    const wire = `team.roles.${role}`;
    // The synthesizer slot is stored as `[team.roles.utility]` (TeamRoles);
    // the RPC accepts either spelling and writes `utility`.
    const table = role === 'synthesizer' && tables['team.roles.utility'] ? 'team.roles.utility' : wire;
    out[`${wire}.runtime`] = tomlStr(tables, table, 'runtime', '');
    out[`${wire}.model`] = tomlStr(tables, table, 'model', '');
    out[`${wire}.effort`] = tomlStr(tables, table, 'effort', '');
  }
  return out;
}

/**
 * 設定 → 自動化: 一員工四角色（全域預設）, `config.toml [team]` (v1.68 W2).
 * Each employee's own `agent.toml [team]` still overrides field by field;
 * the composer reads these per goal, so a save applies to the next goal.
 */
export function TeamDefaultsSection() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const form = useConfigForm((tables) => ({
    'team.enabled': tomlBool(tables, 'team', 'enabled', true),
    'team.gate': tomlStr(tables, 'team', 'gate', 'auto'),
    ...readRoles(tables),
  }));
  const { values, set } = form;

  const gate = vStr(values, 'team.gate') || 'auto';
  const gateOptions: SelectOption[] = [
    ...TEAM_GATES.map((g) => ({ value: g as string, label: t(`settings.team.gate.${g}`), raw: g as string })),
    ...(!(TEAM_GATES as readonly string[]).includes(gate) ? [{ value: gate, label: gate, raw: gate }] : []),
  ];
  const effortOptions: SelectOption[] = EFFORTS.map((e) => ({
    value: e,
    label: e === '' ? t('settings.team.effort.default') : e,
    raw: e,
  }));
  const runtimeOptionsFor = (saved: string): SelectOption[] => [
    ...ROLE_RUNTIMES.map((r) => ({
      value: r as string,
      label: r === '' ? t('settings.team.runtime.default') : t(`settings.team.runtime.${r}`),
      raw: r as string,
    })),
    ...(saved && !(ROLE_RUNTIMES as readonly string[]).includes(saved)
      ? [{ value: saved, label: intl.formatMessage({ id: 'settings.team.runtime.other' }, { name: saved }), raw: saved }]
      : []),
  ];

  const roleRows = (role: TeamRole) => {
    const base = `team.roles.${role}`;
    const runtime = vStr(values, `${base}.runtime`);
    return (
      <SettingsCard key={role}>
        <RowSelect
          label={intl.formatMessage({ id: 'settings.team.role.runtime' }, { role: t(`settings.team.role.${role}`) })}
          value={runtime}
          onChange={(v) => set(`${base}.runtime`, v)}
          options={runtimeOptionsFor(runtime)}
        />
        <SettingsRow label={t('settings.team.role.model')} tier="text">
          <Input
            type="text"
            value={vStr(values, `${base}.model`)}
            onChange={(e) => set(`${base}.model`, e.target.value.trim())}
            placeholder={t('settings.team.role.model.placeholder')}
            aria-label={intl.formatMessage({ id: 'settings.team.role.modelAria' }, { role: t(`settings.team.role.${role}`) })}
          />
        </SettingsRow>
        <RowSelect
          label={t('settings.team.role.effort')}
          value={vStr(values, `${base}.effort`)}
          onChange={(v) => set(`${base}.effort`, v)}
          options={effortOptions}
        />
      </SettingsCard>
    );
  };

  return (
    <SettingsSection title={t('settings.team')} description={t('settings.team.desc')}>
      {form.loadError != null && (
        <ErrorState
          variant="inline"
          error={form.loadError}
          title={t('errorState.manage.loadFailed')}
          onRetry={() => void form.load()}
        />
      )}
      <SettingsCard>
        <RowSwitch
          label={t('settings.team.enabled')}
          description={t('settings.team.enabled.help')}
          checked={vBool(values, 'team.enabled')}
          onChange={(v) => set('team.enabled', v)}
        />
        <RowSelect
          label={t('settings.team.gate')}
          description={t('settings.team.gate.help')}
          value={gate}
          onChange={(v) => set('team.gate', v)}
          options={gateOptions}
        />
      </SettingsCard>
      <AdvancedSection storageKey="settings.automation.teamRoles" label={t('settings.team.roles')}>
        <div className="space-y-3">
          <p className="text-xs text-muted-foreground">{t('settings.team.roles.help')}</p>
          {TEAM_ROLES.map(roleRows)}
        </div>
      </AdvancedSection>
      <div className="flex justify-end">
        <Button variant="brand" size="sm" onClick={() => void form.save()} disabled={form.saving}>
          {form.saving ? t('common.saving') : t('settings.team.save')}
        </Button>
      </div>
    </SettingsSection>
  );
}
