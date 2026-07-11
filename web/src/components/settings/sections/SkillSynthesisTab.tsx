import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { api, type SkillSynthesisConfig } from '@/lib/api';
import { FormField, inputClass, buttonPrimary } from '@/components/shared/Dialog';
import { toast, formatError } from '@/lib/toast';
import { Card } from '@/components/ui';
import { Sparkles, AlertTriangle, ExternalLink } from 'lucide-react';

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
      <Card>
        <p className="py-8 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
      </Card>
    );
  }

  return (
    <Card
      bodyClassName="space-y-6"
      title={
        <span className="flex items-center gap-2">
          <Sparkles className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.skillSynthesis' })}
        </span>
      }
    >
      <p className="text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'skillSynthesis.desc' })}</p>

      {/* Master toggle: auto_run */}
      <label className="flex items-center justify-between gap-3 py-1.5">
        <span>
          <span className="block text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'skillSynthesis.autoRun' })}</span>
          <span className="block text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'skillSynthesis.autoRun.hint' })}</span>
        </span>
        <input
          type="checkbox"
          checked={config.auto_run}
          onChange={(e) => setConfig({ ...config, auto_run: e.target.checked })}
          className="h-4 w-4 shrink-0 accent-amber-500"
        />
      </label>

      {/* dry_run toggle */}
      <label className="flex items-center justify-between gap-3 py-1.5">
        <span>
          <span className="block text-sm text-stone-700 dark:text-stone-300">{intl.formatMessage({ id: 'skillSynthesis.dryRun' })}</span>
          <span className="block text-xs text-stone-400 dark:text-stone-500">{intl.formatMessage({ id: 'skillSynthesis.dryRun.hint' })}</span>
        </span>
        <input
          type="checkbox"
          checked={config.dry_run}
          onChange={(e) => setConfig({ ...config, dry_run: e.target.checked })}
          className="h-4 w-4 shrink-0 accent-amber-500"
        />
      </label>

      {/* Live-mode warning when writes are enabled */}
      {config.auto_run && !config.dry_run && (
        <div className="flex items-start gap-2 rounded-lg border border-amber-300/60 bg-amber-50/60 px-3 py-2 text-xs text-amber-700 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-400">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <div className="space-y-1.5">
            <p>{intl.formatMessage({ id: 'skillSynthesis.liveWarning' })}</p>
            <p>
              {intl.formatMessage({ id: 'skillSynthesis.apiKeyHelp' })}{' '}
              <a
                href="https://console.anthropic.com/settings/keys"
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-0.5 font-medium underline underline-offset-2 hover:text-amber-800 dark:hover:text-amber-300"
              >
                {intl.formatMessage({ id: 'skillSynthesis.apiKeyLink' })}
                <ExternalLink className="h-3 w-3" />
              </a>
            </p>
          </div>
        </div>
      )}

      {/* Scalars */}
      <div className="grid gap-4 sm:grid-cols-2">
        <FormField label={intl.formatMessage({ id: 'skillSynthesis.intervalHours' })} hint="1-168">
          <input
            type="number"
            min={1}
            max={168}
            value={config.interval_hours}
            onChange={(e) => setConfig({ ...config, interval_hours: Number(e.target.value) })}
            className={inputClass}
          />
        </FormField>
        <FormField label={intl.formatMessage({ id: 'skillSynthesis.lookbackDays' })} hint="1-30">
          <input
            type="number"
            min={1}
            max={30}
            value={config.lookback_days}
            onChange={(e) => setConfig({ ...config, lookback_days: Number(e.target.value) })}
            className={inputClass}
          />
        </FormField>
      </div>

      <FormField label={intl.formatMessage({ id: 'skillSynthesis.targetAgent' })} hint={intl.formatMessage({ id: 'skillSynthesis.targetAgent.hint' })}>
        <input
          type="text"
          value={config.target_agent}
          onChange={(e) => setConfig({ ...config, target_agent: e.target.value })}
          placeholder={intl.formatMessage({ id: 'skillSynthesis.targetAgent.placeholder' })}
          className={inputClass}
        />
      </FormField>

      <div className="flex items-center justify-end gap-3 border-t border-[var(--panel-border)] pt-4">
        {saved && <span className="text-sm text-emerald-500">{intl.formatMessage({ id: 'settings.general.saved' })}</span>}
        <button onClick={handleSave} disabled={saving} className={buttonPrimary}>
          {saving ? intl.formatMessage({ id: 'common.saving' }) : intl.formatMessage({ id: 'common.save' })}
        </button>
      </div>
    </Card>
  );
}
