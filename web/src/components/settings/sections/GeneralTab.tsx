import { useCallback, useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useSystemStore } from '@/stores/system-store';
import { useTourStore } from '@/stores/tour-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  ErrorState,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
} from '@/components/mds';
import { AdvancedSection, type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch } from '@/pages/agent-form/form-rows';
import { SettingRow } from './shared';

export function GeneralTab() {
  const intl = useIntl();
  const { status } = useSystemStore();
  const startTour = useTourStore((s) => s.start);
  const [logLevel, setLogLevel] = useState('info');
  const [rotationStrategy, setRotationStrategy] = useState('priority');
  const [defaultLanguage, setDefaultLanguage] = useState('');
  // Login/boot autostart — applied immediately via system.autostart.set (it is
  // an OS-level registration, not a config.toml field, so it bypasses Save).
  // null = unsupported platform or status not loaded yet → row hidden.
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartBusy, setAutostartBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // P05: a failed load used to leave the form showing its hard-coded defaults —
  // indistinguishable from "the gateway really is set to info/priority". Both
  // failure paths now leave a persistent, retryable notice on the page.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(() => {
    setLoadError(null);
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw === 'string') {
        // Parse TOML string for current values
        const logMatch = raw.match(/level\s*=\s*"(\w+)"/);
        if (logMatch) setLogLevel(logMatch[1]);
        const rotMatch = raw.match(/strategy\s*=\s*"(\w+)"/);
        if (rotMatch) setRotationStrategy(rotMatch[1]);
        const langMatch = raw.match(/default_language\s*=\s*"([^"]*)"/);
        if (langMatch) setDefaultLanguage(langMatch[1]);
      }
    }).catch((e) => {
      console.warn("[api]", e);
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
    // Best-effort: non-admin sessions / older gateways just keep the row hidden.
    api.system.autostartStatus().then((s) => {
      if (s.supported) setAutostart(s.enabled);
    }).catch(() => {});
  }, [intl]);

  // Load current config on mount
  useEffect(() => { load(); }, [load]);

  const handleAutostartToggle = async (next: boolean) => {
    setAutostartBusy(true);
    const prev = autostart;
    setAutostart(next);
    try {
      const s = await api.system.autostartSet(next);
      setAutostart(s.enabled);
      toast.success(intl.formatMessage({
        id: next ? 'settings.general.autostart.enabled' : 'settings.general.autostart.disabled',
      }));
    } catch (e) {
      console.warn('[api]', e);
      setAutostart(prev);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setAutostartBusy(false);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    setSaveError(null);
    try {
      await api.system.updateConfig({
        log_level: logLevel,
        rotation_strategy: rotationStrategy,
        default_language: defaultLanguage,
      });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.warn("[api]", e);
      setSaveError(e);
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
  // Empty string = "follow the user's input language" (pre-existing
  // behaviour, unchanged). Non-empty pins a global default reply language.
  const languageOptions: SelectOption[] = ['', 'zh-TW', 'en', 'ja-JP'].map((code) => ({
    value: code,
    label: intl.formatMessage({
      id: code === '' ? 'settings.general.defaultLanguage.followInput' : `settings.general.defaultLanguage.${code}`,
    }),
    raw: code,
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
          {/* Editable: default reply language */}
          <RowSelect
            label={intl.formatMessage({ id: 'settings.general.defaultLanguage' })}
            description={intl.formatMessage({ id: 'settings.general.defaultLanguage.help' })}
            value={defaultLanguage}
            onChange={setDefaultLanguage}
            options={languageOptions}
          />
          {/* Login/boot autostart — hidden when unsupported or not admin */}
          {autostart !== null && (
            <RowSwitch
              label={intl.formatMessage({ id: 'settings.general.autostart' })}
              description={intl.formatMessage({ id: 'settings.general.autostart.help' })}
              checked={autostart}
              onChange={(v) => { if (!autostartBusy) void handleAutostartToggle(v); }}
            />
          )}
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

      {/* Save — a failed write stays on screen next to the button that caused
          it, so the user knows the form still holds unsaved edits. */}
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

function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  if (days > 0) return `${days}d ${hours}h ${minutes}m`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}
