import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { api, type SkillSynthesisConfig } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  SettingsSection,
  SettingsCard,
  SettingsSaveState,
} from '@/components/mds';
import { RowText, RowNumber, RowSwitch } from '@/pages/agent-form/form-rows';
import { AlertTriangle, ExternalLink } from 'lucide-react';

export function SkillSynthesisTab() {
  const intl = useIntl();
  const [config, setConfig] = useState<SkillSynthesisConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  const load = useCallback(async () => {
    try {
      setConfig(await api.skillSynthesis.get());
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    try {
      await api.skillSynthesis.update({
        auto_run: config.auto_run,
        dry_run: config.dry_run,
        interval_hours: config.interval_hours,
        lookback_days: config.lookback_days,
        target_agent: config.target_agent.trim(),
      });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  if (!config) {
    return (
      <p className="py-8 text-center text-sm text-muted-foreground">
        {intl.formatMessage({ id: 'common.loading' })}
      </p>
    );
  }

  return (
    <div className="space-y-8">
      <SettingsSection>
        <SettingsCard>
          <RowSwitch
            label={intl.formatMessage({ id: 'skillSynthesis.autoRun' })}
            description={intl.formatMessage({ id: 'skillSynthesis.autoRun.hint' })}
            checked={config.auto_run}
            onChange={(v) => setConfig({ ...config, auto_run: v })}
          />
          <RowSwitch
            label={intl.formatMessage({ id: 'skillSynthesis.dryRun' })}
            description={intl.formatMessage({ id: 'skillSynthesis.dryRun.hint' })}
            checked={config.dry_run}
            onChange={(v) => setConfig({ ...config, dry_run: v })}
          />
        </SettingsCard>
      </SettingsSection>

      {/* Live-mode warning when writes are enabled */}
      {config.auto_run && !config.dry_run && (
        <div className="flex items-start gap-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <div className="space-y-1.5">
            <p>{intl.formatMessage({ id: 'skillSynthesis.liveWarning' })}</p>
            <p>
              {intl.formatMessage({ id: 'skillSynthesis.apiKeyHelp' })}{' '}
              <a
                href="https://console.anthropic.com/settings/keys"
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-0.5 font-medium underline underline-offset-2 hover:text-warning/80"
              >
                {intl.formatMessage({ id: 'skillSynthesis.apiKeyLink' })}
                <ExternalLink className="h-3 w-3" />
              </a>
            </p>
          </div>
        </div>
      )}

      <SettingsSection>
        <SettingsCard>
          <RowNumber
            label={intl.formatMessage({ id: 'skillSynthesis.intervalHours' })}
            description="1-168"
            value={config.interval_hours}
            min={1}
            max={168}
            onChange={(v) => setConfig({ ...config, interval_hours: v })}
          />
          <RowNumber
            label={intl.formatMessage({ id: 'skillSynthesis.lookbackDays' })}
            description="1-30"
            value={config.lookback_days}
            min={1}
            max={30}
            onChange={(v) => setConfig({ ...config, lookback_days: v })}
          />
          <RowText
            label={intl.formatMessage({ id: 'skillSynthesis.targetAgent' })}
            description={intl.formatMessage({ id: 'skillSynthesis.targetAgent.hint' })}
            value={config.target_agent}
            onChange={(v) => setConfig({ ...config, target_agent: v })}
            placeholder={intl.formatMessage({ id: 'skillSynthesis.targetAgent.placeholder' })}
          />
        </SettingsCard>
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
