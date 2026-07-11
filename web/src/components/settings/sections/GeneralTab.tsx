import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useSystemStore } from '@/stores/system-store';
import { useTourStore } from '@/stores/tour-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Card, Button } from '@/components/ui';
import { AdvancedSection, OptionSelect, SettingField, type SelectOption } from '@/components/settings/controls';
import { SettingRow } from './shared';

export function GeneralTab() {
  const intl = useIntl();
  const { status } = useSystemStore();
  const startTour = useTourStore((s) => s.start);
  const [logLevel, setLogLevel] = useState('info');
  const [rotationStrategy, setRotationStrategy] = useState('priority');
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  // Load current config on mount
  useEffect(() => {
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw === 'string') {
        // Parse TOML string for current values
        const logMatch = raw.match(/level\s*=\s*"(\w+)"/);
        if (logMatch) setLogLevel(logMatch[1]);
        const rotMatch = raw.match(/strategy\s*=\s*"(\w+)"/);
        if (rotMatch) setRotationStrategy(rotMatch[1]);
      }
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    try {
      await api.system.updateConfig({ log_level: logLevel, rotation_strategy: rotationStrategy });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const logLevelOptions: SelectOption[] = ['trace', 'debug', 'info', 'warn', 'error'].map((l) => ({
    value: l,
    label: intl.formatMessage({ id: `settings.logLevel.${l}` }),
    raw: l,
  }));
  const rotationOptions: SelectOption[] = ['priority', 'round_robin', 'least_cost', 'failover'].map((s) => ({
    value: s,
    label: intl.formatMessage({ id: `settings.rotation.${s}` }),
    raw: s,
  }));

  return (
    <Card title={intl.formatMessage({ id: 'settings.general' })}>
      <div className="space-y-4">
        <SettingRow
          label={intl.formatMessage({ id: 'settings.general.gatewayAddress' })}
          value={status?.gateway_address ?? '0.0.0.0:3100'}
        />
        <SettingRow label={intl.formatMessage({ id: 'settings.general.version' })} value={status?.version ?? '-'} />
        <SettingRow
          label={intl.formatMessage({ id: 'settings.general.uptime' })}
          value={status?.uptime_seconds ? formatUptime(status.uptime_seconds) : '-'}
        />

        {/* Editable: Log Level */}
        <SettingField
          layout="row"
          label={intl.formatMessage({ id: 'settings.general.logLevel' })}
          help={intl.formatMessage({ id: 'settings.general.logLevel.help' })}
        >
          <OptionSelect
            value={logLevel}
            onChange={setLogLevel}
            options={logLevelOptions}
            className="w-auto min-w-[12rem]"
          />
        </SettingField>

        {/* Replay the guided tour */}
        <SettingField layout="row" label={intl.formatMessage({ id: 'settings.general.replayTour' })}>
          <Button variant="secondary" onClick={() => startTour()}>
            {intl.formatMessage({ id: 'settings.general.replayTour.button' })}
          </Button>
        </SettingField>

        {/* Advanced: account rotation strategy — everyday users don't touch it */}
        <AdvancedSection storageKey="settings.general">
          <SettingField
            layout="row"
            label={intl.formatMessage({ id: 'settings.general.rotationStrategy' })}
            help={intl.formatMessage({ id: 'settings.general.rotationStrategy.help' })}
          >
            <OptionSelect
              value={rotationStrategy}
              onChange={setRotationStrategy}
              options={rotationOptions}
              className="w-auto min-w-[14rem]"
            />
          </SettingField>
        </AdvancedSection>

        {/* Save button */}
        <div className="flex items-center justify-end gap-2 pt-2">
          {saved && (
            <span className="text-xs text-emerald-600 dark:text-emerald-400">
              {intl.formatMessage({ id: 'settings.general.saved' })}
            </span>
          )}
          <Button variant="primary" onClick={handleSave} disabled={saving}>
            {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </div>
      </div>
    </Card>
  );
}

function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h ${minutes}m`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}
