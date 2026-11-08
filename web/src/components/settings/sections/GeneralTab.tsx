import { useCallback, useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { useAuthStore } from '@/stores/auth-store';
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
import { type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch, RowText } from '@/pages/agent-form/form-rows';
import { SettingRow } from './shared';
import { changedPayload, parseTomlSubset, tomlStr, type FlatValues } from '@/lib/config-toml';
import { isValidPublicUrl } from '@/lib/dashboard-base';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

export function GeneralTab() {
  const intl = useIntl();
  const { status } = useSystemStore();
  const startTour = useTourStore((s) => s.start);
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  const [defaultLanguage, setDefaultLanguage] = useState('');
  // RM-129: `[dashboard] public_url` — the externally reachable dashboard
  // address (sub-path included) used for OAuth callbacks and return links.
  const [publicUrl, setPublicUrl] = useState('');
  const publicUrlValid = isValidPublicUrl(publicUrl);
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
  // v1.68: loaded values — a save sends only what changed, so an untouched
  // boot-only log level does not raise a "restart required" notice.
  const initialRef = useRef<FlatValues>({});

  const load = useCallback(() => {
    setLoadError(null);
    api.system.config().then((res) => {
      const raw = (res as Record<string, unknown>)?.config;
      if (typeof raw === 'string') {
        // v1.68: read each key from its own section (the old unanchored
        // regexes could pick up `level` / `strategy` from another table).
        const tables = parseTomlSubset(raw);
        const language = tomlStr(tables, 'general', 'default_language', '');
        const publicUrlValue = tomlStr(tables, 'dashboard', 'public_url', '');
        setDefaultLanguage(language);
        setPublicUrl(publicUrlValue);
        initialRef.current = { default_language: language, 'dashboard.public_url': publicUrlValue };
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
    const current: FlatValues = {
      default_language: defaultLanguage,
      ...(isAdmin ? { 'dashboard.public_url': publicUrl.trim() } : {}),
    };
    if (isAdmin && !publicUrlValid) {
      toast.error(intl.formatMessage({ id: 'settings.general.publicUrl.invalid' }));
      return;
    }
    const payload = changedPayload(initialRef.current, current);
    if (Object.keys(payload).length === 0) {
      toast.info(intl.formatMessage({ id: 'settings.noChanges' }));
      return;
    }
    setSaving(true);
    setSaved(false);
    setSaveError(null);
    try {
      const res = await api.system.updateConfig(payload);
      initialRef.current = current;
      const restart = res?.restart_required ?? [];
      if (restart.length > 0) useRestartRequiredStore.getState().add(restart);
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

  // Empty string = "follow the user's input language" (pre-existing
  // behaviour, unchanged). Non-empty pins a global default reply language.
  const languageOptions: SelectOption[] = ['', 'zh-TW', 'zh-CN', 'en', 'ja-JP'].map((code) => ({
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
          {/* Editable: default reply language */}
          <RowSelect
            label={intl.formatMessage({ id: 'settings.general.defaultLanguage' })}
            description={intl.formatMessage({ id: 'settings.general.defaultLanguage.help' })}
            value={defaultLanguage}
            onChange={setDefaultLanguage}
            options={languageOptions}
          />
          {/* RM-129: dashboard public URL (admin only; validated again by the gateway) */}
          {isAdmin && (
            <RowText
              label={intl.formatMessage({ id: 'settings.general.publicUrl' })}
              description={
                <>
                  {intl.formatMessage({ id: 'settings.general.publicUrl.help' })}
                  {!publicUrlValid && (
                    <span role="alert" className="mt-1 block text-red-600 dark:text-red-400">
                      {intl.formatMessage({ id: 'settings.general.publicUrl.invalid' })}
                    </span>
                  )}
                </>
              }
              value={publicUrl}
              onChange={setPublicUrl}
              placeholder="https://example.com/agents/"
              autoComplete="off"
            />
          )}
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

      {isAdmin && (
        <div className="flex flex-wrap gap-4 text-sm">
          <Link className="text-primary underline underline-offset-4" to="/app/system/settings/advanced#rotation">
            {intl.formatMessage({ id: 'settings.general.rotationStrategy' })}
          </Link>
          <Link className="text-primary underline underline-offset-4" to="/app/system/settings/advanced#logging">
            {intl.formatMessage({ id: 'settings.general.logLevel' })}
          </Link>
        </div>
      )}

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
