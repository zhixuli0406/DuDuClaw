import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type AdminAiSettings } from '@/lib/api';
import { formatDateTime } from '@/lib/company-time';
import { settingsRefusal } from '@/lib/settings-refusal';
import { useUserNames } from '@/lib/use-user-names';
import { useAuthStore } from '@/stores/auth-store';
import { Button, SettingsCard, SettingsSection, Switch, useErrorMessage } from '@/components/mds';
import { RiskConfirmDialog } from '@/components/settings/controls';

const ALLOWED = ['schedule', 'skills', 'model', 'fallback', 'thinking', 'runtime', 'steps', 'idle', 'wake', 'pause', 'name', 'icon', 'callName', 'language'];
const LOCAL_ONLY = ['permissions', 'channels', 'budget', 'engine', 'google', 'odoo', 'organization', 'delegation', 'tools', 'personality', 'archive', 'install'];

export function AdminAiCard() {
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');
  return isAdmin ? <AdminAiContent /> : null;
}

function AdminAiContent() {
  const intl = useIntl();
  const t = (key: string) => intl.formatMessage({ id: `settings.adminAi.${key}` });
  const errorText = useErrorMessage();
  const { nameOf } = useUserNames();
  const [settings, setSettings] = useState<AdminAiSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const saving = useRef(false);
  const epoch = useRef(0);
  const load = useCallback(async () => {
    const current = ++epoch.current;
    setLoading(true);
    setLoadError(null);
    try {
      const next = await api.adminAi.get();
      if (current === epoch.current) setSettings(next);
    } catch (e) {
      if (current === epoch.current) setLoadError(e);
    } finally {
      if (current === epoch.current) setLoading(false);
    }
  }, []);
  useEffect(() => {
    void load();
    return () => { epoch.current += 1; };
  }, [load]);

  const save = async (enabled: boolean) => {
    if (!settings || loading || saving.current) return;
    saving.current = true;
    setBusy(true);
    setSaveError(null);
    try {
      const next = await api.adminAi.set(enabled ? { enabled: true, acknowledge_risk: true } : { enabled: false });
      setSettings(next);
      setOpen(false);
    } catch (e) {
      setSaveError(e);
    } finally {
      saving.current = false;
      setBusy(false);
    }
  };

  const invalidConfig = settings?.config_state === 'unreadable' || settings?.config_state === 'malformed';
  const enabled = !invalidConfig && (settings?.enabled ?? false);

  return (
    <SettingsSection title={t('title')} description={t('description')}>
      <SettingsCard>
        <div className="space-y-3 px-4 py-4">
          <div className="flex items-center justify-between gap-4">
            <label htmlFor="admin-ai-switch" className="text-sm font-medium">{t('title')}</label>
            <Switch id="admin-ai-switch" checked={enabled} disabled={loading || loadError != null || !settings || busy}
              onCheckedChange={(enabled) => {
                setSaveError(null);
                if (enabled) setOpen(true);
                else void save(false);
              }} />
          </div>
          {invalidConfig && <p className="text-sm text-muted-foreground">{t('invalidConfig')}</p>}
          {loading && <p role="status" className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'common.loading' })}</p>}
          {loadError != null && <div role="alert" className="space-y-2 text-sm text-destructive">
            <p>{errorText(loadError)}</p>
            <Button variant="outline" size="sm" onClick={() => void load()}>{intl.formatMessage({ id: 'errorState.retry' })}</Button>
          </div>}
          {saveError != null && !open && <p role="alert" className="text-sm text-destructive">{settingsRefusal(saveError, errorText(saveError))}</p>}
          {enabled && settings?.enabled_ack && <p className="text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'settings.adminAi.lastEnabled' }, { name: settings.enabled_ack.by_name?.trim() || nameOf(settings.enabled_ack.by), time: formatDateTime(settings.enabled_ack.at, { locale: intl.locale }) })}
          </p>}
        </div>
        <div className="grid gap-6 px-4 py-4 sm:grid-cols-2">
          <div className="space-y-2">
            <h4 className="text-sm font-medium">{t('allowed')}</h4>
            <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">{ALLOWED.map((item) => <li key={item}>{t(`allowed.${item}`)}</li>)}</ul>
            <p className="text-xs text-muted-foreground">{t('scheduleNote')}</p>
          </div>
          <div className="space-y-2">
            <h4 className="text-sm font-medium">{t('localOnly')}</h4>
            <ul className="list-disc space-y-1 pl-5 text-sm text-muted-foreground">{LOCAL_ONLY.map((item) => <li key={item}>{t(`localOnly.${item}`)}</li>)}</ul>
          </div>
        </div>
        <p className="bg-muted/40 px-4 py-3 text-xs text-muted-foreground">{t('auditNote')}</p>
      </SettingsCard>
      <RiskConfirmDialog open={open} onClose={() => { if (!busy) setOpen(false); }} onConfirm={() => void save(true)}
        title={t('confirmTitle')} body={t('confirmBody')} busy={busy}
        error={saveError == null ? null : settingsRefusal(saveError, errorText(saveError))} />
    </SettingsSection>
  );
}
