import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Info } from 'lucide-react';
import { api, type TaskForwardModelSettings } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';
import {
  Button,
  ErrorState,
  SettingsSection,
  SettingsCard,
  SettingsSaveState,
} from '@/components/mds';
import { RowSwitch } from '@/pages/agent-form/form-rows';
import { AutonomyNote } from '@/components/AutonomyNote';

/**
 * 校準式預測與學習閘 (v1.54) — the three global `[task_forward_model]`
 * switches, layered: the master switch turns on task-level prediction; the
 * second scores each prediction against the real outcome; the third makes
 * inductive lessons earn adoption out-of-sample before they reach the prompt.
 *
 * Owner/admin only. The gateway gates `task_forward_model.get/set` too — this
 * component rendering `null` for everyone else is the courtesy half, not the
 * enforcement. See docs/features/39-calibrated-forward-model.md.
 */
export function CalibrationTab() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const isAdmin = useAuthStore((s) => s.user?.role === 'admin');

  const [config, setConfig] = useState<TaskForwardModelSettings>({
    enabled: false,
    calibration_enabled: false,
    held_out_gate_enabled: false,
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  // P05: a failed read left all three switches showing `false`, i.e. exactly
  // what a deliberately-disabled forward model looks like. Say which it is.
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saveError, setSaveError] = useState<unknown>(null);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const settings = await api.taskForwardModel.get();
      setConfig(settings);
    } catch (e) {
      console.warn('[api]', e);
      setLoadError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    } finally {
      setLoading(false);
    }
  }, [intl]);

  useEffect(() => {
    if (!isAdmin) {
      setLoading(false);
      return;
    }
    void load();
  }, [isAdmin, load]);

  if (!isAdmin) return null;

  const handleSave = async () => {
    setSaving(true);
    setSaved(false);
    setSaveError(null);
    try {
      const res = await api.taskForwardModel.set(config);
      setConfig({
        enabled: res.enabled,
        calibration_enabled: res.calibration_enabled,
        held_out_gate_enabled: res.held_out_gate_enabled,
      });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
      if (res.enabled_requires_restart) {
        toast.info(t('settings.calibration.restartHint'));
      }
    } catch (e) {
      console.warn('[api]', e);
      setSaveError(e);
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-8">
      {loadError != null && (
        <ErrorState
          variant="inline"
          error={loadError}
          title={intl.formatMessage({ id: 'errorState.manage.loadFailed' })}
          onRetry={() => void load()}
        />
      )}

      <SettingsSection
        title={t('settings.calibration.section')}
        description={t('settings.calibration.section.desc')}
      >
        <SettingsCard>
          <RowSwitch
            label={t('settings.calibration.enabled')}
            description={t('settings.calibration.enabled.help')}
            checked={config.enabled}
            disabled={loading}
            onChange={(v) => setConfig((c) => ({ ...c, enabled: v }))}
          />
          <RowSwitch
            label={t('settings.calibration.scoring')}
            description={t('settings.calibration.scoring.help')}
            checked={config.calibration_enabled}
            disabled={loading || !config.enabled}
            onChange={(v) => setConfig((c) => ({ ...c, calibration_enabled: v }))}
          />
          <RowSwitch
            label={t('settings.calibration.gate')}
            description={t('settings.calibration.gate.help')}
            checked={config.held_out_gate_enabled}
            disabled={loading || !config.enabled}
            onChange={(v) => setConfig((c) => ({ ...c, held_out_gate_enabled: v }))}
          />
        </SettingsCard>
        {config.enabled ? (
          <AutonomyNote id="calibration" />
        ) : (
          <p className="flex items-start gap-2 rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
            <Info className="mt-0.5 size-3.5 shrink-0" />
            {t('settings.calibration.masterOffHint')}
          </p>
        )}
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
      <div className="flex flex-wrap items-center justify-end gap-3">
        <span className="mr-auto text-xs text-muted-foreground">
          {t('settings.calibration.applied')}
        </span>
        <SettingsSaveState
          status={saving ? 'saving' : saved ? 'saved' : 'idle'}
          savingLabel={intl.formatMessage({ id: 'common.saving' })}
          savedLabel={intl.formatMessage({ id: 'settings.general.saved' })}
        />
        <Button variant="brand" size="sm" onClick={handleSave} disabled={loading || saving}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </div>
  );
}
