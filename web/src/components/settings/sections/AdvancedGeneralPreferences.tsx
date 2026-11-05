import { useIntl } from 'react-intl';
import { Button, ErrorState, SettingsCard } from '@/components/mds';
import { RowSelect } from '@/pages/agent-form/form-rows';
import { tomlStr } from '@/lib/config-toml';
import { useConfigForm, vStr } from '../useConfigForm';

/** Rarely changed general preferences, using the same diff-based config writer. */
export function AdvancedGeneralPreferences({ field }: { field: 'logging' | 'rotation' }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const logging = field === 'logging';
  const key = logging ? 'log_level' : 'rotation_strategy';
  const form = useConfigForm((tables) => ({
    [key]: logging ? tomlStr(tables, 'general', 'log_level', 'info') : tomlStr(tables, 'rotation', 'strategy', 'priority'),
  }));
  const choices = logging ? ['trace', 'debug', 'info', 'warn', 'error'] : ['priority', 'round_robin', 'least_cost', 'failover'];
  return (
    <div className="space-y-4">
      {form.loadError != null && <ErrorState variant="inline" error={form.loadError} onRetry={() => void form.load()} />}
      <SettingsCard>
        <RowSelect
          label={t(logging ? 'settings.general.logLevel' : 'settings.general.rotationStrategy')}
          description={t(logging ? 'settings.general.logLevel.liveNote' : 'settings.general.rotationStrategy.help')}
          value={vStr(form.values, key)}
          onChange={(value) => form.set(key, value)}
          options={choices.map((value) => ({ value, raw: value, label: t(`${logging ? 'settings.logLevel' : 'settings.rotation'}.${value}`) }))}
        />
      </SettingsCard>
      {form.saveError != null && <ErrorState variant="inline" error={form.saveError} />}
      <div className="flex justify-end">
        <Button variant="brand" onClick={() => void form.save()} disabled={!form.loaded || form.loadError != null || form.saving}>
          {t(form.saving ? 'common.saving' : 'common.save')}
        </Button>
      </div>
    </div>
  );
}
