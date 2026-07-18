import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useSystemStore } from '@/stores/system-store';
import { useTourStore } from '@/stores/tour-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
} from '@/components/mds';
import { AdvancedSection, type SelectOption } from '@/components/settings/controls';
import { RowSelect } from '@/pages/agent-form/form-rows';
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
    <div className="space-y-8">
      <SettingsSection>
        <SettingsCard>
          <SettingRow
            label={intl.formatMessage({ id: 'settings.general.gatewayAddress' })}
            value={status?.gateway_address ?? '0.0.0.0:3100'}
          />
          <SettingRow label={intl.formatMessage({ id: 'settings.general.version' })} value={status?.version ?? '-'} />
          <SettingRow
            label={intl.formatMessage({ id: 'settings.general.uptime' })}
            value={status?.uptime_seconds ? formatUptime(status.uptime_seconds) : '-'}
          />
        </SettingsCard>
      </SettingsSection>

      <SettingsSection>
        <SettingsCard>
          {/* Editable: Log Level */}
          <RowSelect
            label={intl.formatMessage({ id: 'settings.general.logLevel' })}
            description={intl.formatMessage({ id: 'settings.general.logLevel.help' })}
            value={logLevel}
            onChange={setLogLevel}
            options={logLevelOptions}
          />
          {/* Replay the guided tour */}
          <SettingsRow label={intl.formatMessage({ id: 'settings.general.replayTour' })}>
            <Button variant="outline" size="sm" onClick={() => startTour()}>
              {intl.formatMessage({ id: 'settings.general.replayTour.button' })}
            </Button>
          </SettingsRow>
        </SettingsCard>
      </SettingsSection>

      {/* Advanced: account rotation strategy — everyday users don't touch it */}
      <AdvancedSection storageKey="settings.general">
        <SettingsCard>
          <RowSelect
            label={intl.formatMessage({ id: 'settings.general.rotationStrategy' })}
            description={intl.formatMessage({ id: 'settings.general.rotationStrategy.help' })}
            value={rotationStrategy}
            onChange={setRotationStrategy}
            options={rotationOptions}
          />
        </SettingsCard>
      </AdvancedSection>

      {/* Save */}
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

function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h ${minutes}m`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}
