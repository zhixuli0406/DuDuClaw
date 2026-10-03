import { useEffect } from 'react';
import { useIntl } from 'react-intl';
import { Button, ErrorState, SettingsCard, SettingsSection } from '@/components/mds';
import { type SelectOption } from '@/components/settings/controls';
import { RowNumber, RowSelect, RowSwitch } from '@/pages/agent-form/form-rows';
import { tomlBool, tomlNum, tomlStr } from '@/lib/config-toml';
import { useAgentsStore } from '@/stores/agents-store';
import { useConfigForm, vBool, vNum, vStr } from '../useConfigForm';

// `[takeover]` defaults — duduclaw_core::takeover_state (DEFAULT_DURATION_MINUTES
// 60, HARD_MAX_DURATION_MINUTES 12 h). Off unless the operator opts in.
const TAKEOVER_DEFAULT_MINUTES = 60;
const TAKEOVER_HARD_MAX_MINUTES = 720;

/**
 * 設定 → 自動化: 真人接手 (`[takeover]`) and AI 員工信箱 (`[mail]`), v1.68 W2.
 * Both are re-read per use by the gateway, so a save is effective at once.
 */
export function TakeoverMailSection() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const { agents, fetchAgents } = useAgentsStore();
  useEffect(() => {
    fetchAgents();
  }, [fetchAgents]);

  const form = useConfigForm((tables) => ({
    'takeover.enabled': tomlBool(tables, 'takeover', 'enabled', false),
    'takeover.duration_minutes': tomlNum(tables, 'takeover', 'duration_minutes', TAKEOVER_DEFAULT_MINUTES),
    'takeover.max_duration_minutes': tomlNum(tables, 'takeover', 'max_duration_minutes', TAKEOVER_HARD_MAX_MINUTES),
    'mail.enabled': tomlBool(tables, 'mail', 'enabled', false),
    'mail.gmail_enabled': tomlBool(tables, 'mail', 'gmail_enabled', false),
    // Reader default (`MailConfig`): the drop folder is on when the key is absent.
    'mail.dropfolder_enabled': tomlBool(tables, 'mail', 'dropfolder_enabled', true),
    'mail.default_agent': tomlStr(tables, 'mail', 'default_agent', ''),
    'mail.auto_trigger': tomlBool(tables, 'mail', 'auto_trigger', false),
  }));
  const { values, set } = form;

  const duration = vNum(values, 'takeover.duration_minutes', TAKEOVER_DEFAULT_MINUTES);
  const maxDuration = vNum(values, 'takeover.max_duration_minutes', TAKEOVER_HARD_MAX_MINUTES);
  const durationInvalid = duration < 1 || maxDuration < 1 || duration > maxDuration || maxDuration > TAKEOVER_HARD_MAX_MINUTES;

  const agentOptions: SelectOption[] = [
    { value: '', label: t('settings.mail.defaultAgent.none'), raw: '' },
    ...agents.map((a) => ({ value: a.name, label: a.display_name || a.name, raw: a.name })),
  ];
  const savedAgent = vStr(values, 'mail.default_agent');
  if (savedAgent && !agentOptions.some((o) => o.value === savedAgent)) {
    agentOptions.push({ value: savedAgent, label: savedAgent, raw: savedAgent });
  }

  const onSave = () => {
    if (durationInvalid) return;
    void form.save();
  };

  return (
    <div className="space-y-8">
      {form.loadError != null && (
        <ErrorState
          variant="inline"
          error={form.loadError}
          title={t('errorState.manage.loadFailed')}
          onRetry={() => void form.load()}
        />
      )}
      <SettingsSection title={t('settings.takeover')} description={t('settings.takeover.desc')}>
        <SettingsCard>
          <RowSwitch
            label={t('settings.takeover.enabled')}
            description={t('settings.takeover.enabled.help')}
            checked={vBool(values, 'takeover.enabled')}
            onChange={(v) => set('takeover.enabled', v)}
          />
          <RowNumber
            label={t('settings.takeover.duration')}
            description={t('settings.takeover.duration.help')}
            value={duration}
            min={1}
            max={TAKEOVER_HARD_MAX_MINUTES}
            onChange={(v) => set('takeover.duration_minutes', v)}
          />
          <RowNumber
            label={t('settings.takeover.maxDuration')}
            description={t('settings.takeover.maxDuration.help')}
            value={maxDuration}
            min={1}
            max={TAKEOVER_HARD_MAX_MINUTES}
            onChange={(v) => set('takeover.max_duration_minutes', v)}
          />
        </SettingsCard>
        {durationInvalid && (
          <p role="alert" className="text-xs text-destructive">
            {intl.formatMessage({ id: 'settings.takeover.durationInvalid' }, { max: TAKEOVER_HARD_MAX_MINUTES })}
          </p>
        )}
        <p className="rounded-md bg-secondary px-3 py-2 text-xs text-muted-foreground">
          {t('settings.takeover.enterpriseNote')}
        </p>
      </SettingsSection>

      <SettingsSection title={t('settings.mail')} description={t('settings.mail.desc')}>
        <SettingsCard>
          <RowSwitch
            label={t('settings.mail.enabled')}
            description={t('settings.mail.enabled.help')}
            checked={vBool(values, 'mail.enabled')}
            onChange={(v) => set('mail.enabled', v)}
          />
          <RowSwitch
            label={t('settings.mail.gmail')}
            description={t('settings.mail.gmail.help')}
            checked={vBool(values, 'mail.gmail_enabled')}
            onChange={(v) => set('mail.gmail_enabled', v)}
          />
          <RowSwitch
            label={t('settings.mail.dropfolder')}
            description={t('settings.mail.dropfolder.help')}
            checked={vBool(values, 'mail.dropfolder_enabled')}
            onChange={(v) => set('mail.dropfolder_enabled', v)}
          />
          <RowSelect
            label={t('settings.mail.defaultAgent')}
            description={t('settings.mail.defaultAgent.help')}
            value={savedAgent}
            onChange={(v) => set('mail.default_agent', v)}
            options={agentOptions}
          />
          <RowSwitch
            label={t('settings.mail.autoTrigger')}
            description={t('settings.mail.autoTrigger.help')}
            checked={vBool(values, 'mail.auto_trigger')}
            onChange={(v) => set('mail.auto_trigger', v)}
          />
        </SettingsCard>
        <div className="flex justify-end">
          <Button variant="brand" size="sm" onClick={onSave} disabled={form.saving || durationInvalid}>
            {form.saving ? t('common.saving') : t('settings.takeoverMail.save')}
          </Button>
        </div>
      </SettingsSection>
    </div>
  );
}
