import { useIntl } from 'react-intl';
import { Button, ErrorState, Input, SettingsCard, SettingsRow, SettingsSection } from '@/components/mds';
import { RowNumber } from '@/pages/agent-form/form-rows';
import { type TomlTables } from '@/lib/config-toml';
import { useConfigForm, vNum } from '../useConfigForm';

const GROUPS = [
  { id: 'runtime', fields: [
    ['max_timeout_secs', 3600, 60, 14400],
  ] },
  { id: 'goal_defaults', fields: [
    ['assign_schedule_max_active', 20, 0, 200],
    ['assign_schedule_min_interval_minutes', 60, 60, 10080],
  ] },
  { id: 'agent_files', fields: [
    ['review_daily_limit', 200, 0, 100000],
    ['review_daily_limit_per_user', 40, 0, 100000],
    ['review_daily_limit_per_agent', 60, 0, 100000],
    ['checks_per_minute', 10, 1, 1000],
    ['max_pending_per_user', 20, 1, 200],
  ] },
] as const;

function readNumber(tables: TomlTables, section: string, key: string, fallback: number) {
  const value = tables[section]?.[key];
  return typeof value === 'number' ? value : fallback;
}

function LimitsCard({ group }: { group: (typeof GROUPS)[number] }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id: `settings.limits.${id}` });
  const form = useConfigForm((tables) => ({
    ...Object.fromEntries(group.fields.map(([key, fallback]) => [
      `${group.id}.${key}`, readNumber(tables, group.id, key, fallback),
    ])),
    ...(group.id === 'runtime' ? { 'runtime.default_timeout_secs': tables.runtime?.default_timeout_secs ?? null } : {}),
  }));
  const max = vNum(form.values, 'runtime.max_timeout_secs', 3600);
  return (
    <SettingsSection title={t(group.id)} description={t(`${group.id}.help`)}>
      {form.loadError != null && <ErrorState variant="inline" error={form.loadError} onRetry={() => void form.load()} />}
      <SettingsCard>
        {group.id === 'runtime' && (
          <SettingsRow label={t('default_timeout_secs')} description={t('default_timeout_secs.help')} tier="text">
            <Input type="number" aria-label={t('default_timeout_secs')} min={30} max={max} step={1}
              value={typeof form.values['runtime.default_timeout_secs'] === 'number' ? String(form.values['runtime.default_timeout_secs']) : ''}
              onChange={(e) => form.set('runtime.default_timeout_secs', e.target.value === '' ? null : Number(e.target.value))} />
          </SettingsRow>
        )}
        {group.fields.map(([key, fallback, min, upper]) => (
          <RowNumber key={key} label={t(key)} description={t(`${key}.help`)}
            value={vNum(form.values, `${group.id}.${key}`, fallback)} min={min} max={upper} step={1}
            onChange={(v) => form.set(`${group.id}.${key}`, v)} />
        ))}
      </SettingsCard>
      {form.saveError != null && <ErrorState variant="inline" error={form.saveError} />}
      <div className="flex justify-end">
        <Button variant="brand" size="sm" disabled={!form.loaded || form.saving} onClick={() => void form.save()}>
          {t(`${group.id}.save`)}
        </Button>
      </div>
    </SettingsSection>
  );
}

export function RunLimitsSections() {
  return <>{GROUPS.map((group) => <LimitsCard key={group.id} group={group} />)}</>;
}
