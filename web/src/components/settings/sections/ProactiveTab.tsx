import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Card, Button, controlClass } from '@/components/ui';
import { AdvancedSection, OptionSelect, ScheduleBuilder, SettingField, Switch, type SelectOption } from '@/components/settings/controls';

// ── Proactive Settings Tab ─────────────────────────────────────

export function ProactiveTab() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState('');
  const [config, setConfig] = useState({
    enabled: false,
    check_interval: '*/30 * * * *',
    quiet_hours_start: 23,
    quiet_hours_end: 8,
    max_messages_per_hour: 3,
    notify_channel: '',
    notify_chat_id: '',
  });
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  useEffect(() => { fetchAgents(); }, [fetchAgents]);
  useEffect(() => {
    if (agents.length > 0 && !selectedAgent) setSelectedAgent(agents[0].name);
  }, [agents, selectedAgent]);

  // Proactive settings live in each agent's agent.toml [proactive] section.
  useEffect(() => {
    if (!selectedAgent) return;
    api.agents.inspect(selectedAgent).then((detail) => {
      // Always reset — switching to an agent without a [proactive] section
      // must not carry over the previous agent's values.
      setConfig({
        enabled: detail?.proactive?.enabled ?? false,
        check_interval: detail?.proactive?.check_interval ?? '*/30 * * * *',
        quiet_hours_start: detail?.proactive?.quiet_hours_start ?? 23,
        quiet_hours_end: detail?.proactive?.quiet_hours_end ?? 8,
        max_messages_per_hour: detail?.proactive?.max_messages_per_hour ?? 3,
        notify_channel: detail?.proactive?.notify_channel ?? '',
        notify_chat_id: detail?.proactive?.notify_chat_id ?? '',
      });
    }).catch((e) => {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [selectedAgent, intl]);

  const handleSave = async () => {
    if (!selectedAgent) return;
    setSaving(true);
    try {
      await api.agents.update(selectedAgent, { proactive: config });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const channelOptions: SelectOption[] = [
    { value: '', label: intl.formatMessage({ id: 'proactive.selectChannel' }) },
    { value: 'telegram', label: 'Telegram' },
    { value: 'line', label: 'LINE' },
    { value: 'discord', label: 'Discord' },
  ];

  return (
    <Card
      bodyClassName="space-y-6"
      title={intl.formatMessage({ id: 'proactive.title' })}
      actions={
        <select
          value={selectedAgent}
          onChange={(e) => setSelectedAgent(e.target.value)}
          className={cn(controlClass, 'h-8 w-auto min-w-[8rem] text-xs')}
        >
          {agents.length === 0 && <option value="">{intl.formatMessage({ id: 'common.noData' })}</option>}
          {agents.map((a) => (
            <option key={a.name} value={a.name}>{a.display_name || a.name}</option>
          ))}
        </select>
      }
    >
      <SettingField
        layout="row"
        label={intl.formatMessage({ id: config.enabled ? 'proactive.enabled' : 'proactive.disabled' })}
        help={intl.formatMessage({ id: 'proactive.enabled.help' })}
      >
        <Switch
          checked={config.enabled}
          onChange={(v) => setConfig({ ...config, enabled: v })}
          label={intl.formatMessage({ id: 'proactive.enabled' })}
        />
      </SettingField>

      <div className="grid gap-4 sm:grid-cols-2">
        <SettingField
          label={intl.formatMessage({ id: 'proactive.checkInterval' })}
          help={intl.formatMessage({ id: 'proactive.checkInterval.help2' })}
          className="sm:col-span-2"
        >
          <ScheduleBuilder
            value={config.check_interval}
            onChange={(cron) => setConfig({ ...config, check_interval: cron })}
          />
        </SettingField>

        <SettingField label={intl.formatMessage({ id: 'proactive.quietHours' })} help={intl.formatMessage({ id: 'proactive.quietHours.help' })}>
          <div className="flex items-center gap-2">
            <input type="number" min={0} max={23} value={config.quiet_hours_start}
              onChange={(e) => setConfig({ ...config, quiet_hours_start: +e.target.value })}
              className={cn(controlClass, 'w-16 px-2 text-center')} />
            <span className="text-stone-400">—</span>
            <input type="number" min={0} max={23} value={config.quiet_hours_end}
              onChange={(e) => setConfig({ ...config, quiet_hours_end: +e.target.value })}
              className={cn(controlClass, 'w-16 px-2 text-center')} />
          </div>
        </SettingField>

        <SettingField label={intl.formatMessage({ id: 'proactive.maxMessagesPerHour' })} help={intl.formatMessage({ id: 'proactive.maxMessagesPerHour.help' })}>
          <input type="number" min={1} max={60} value={config.max_messages_per_hour}
            onChange={(e) => setConfig({ ...config, max_messages_per_hour: +e.target.value })}
            className={cn(controlClass, 'w-24')} />
        </SettingField>

        <SettingField label={intl.formatMessage({ id: 'proactive.notifyChannel' })} help={intl.formatMessage({ id: 'proactive.notifyChannel.help' })}>
          <OptionSelect
            value={config.notify_channel}
            onChange={(v) => setConfig({ ...config, notify_channel: v })}
            showRaw={false}
            options={channelOptions}
          />
        </SettingField>
      </div>

      <AdvancedSection storageKey="settings.proactive" label={intl.formatMessage({ id: 'proactive.advanced' })}>
        <SettingField label={intl.formatMessage({ id: 'proactive.chatId' })} help={intl.formatMessage({ id: 'proactive.chatId.help' })}>
          <input type="text" value={config.notify_chat_id}
            onChange={(e) => setConfig({ ...config, notify_chat_id: e.target.value })}
            placeholder="e.g., 123456789"
            className={controlClass} />
        </SettingField>
      </AdvancedSection>

      <div className="flex justify-end pt-2">
        <Button variant="primary" onClick={handleSave} disabled={saving}>
          {saved
            ? intl.formatMessage({ id: 'settings.general.saved' })
            : saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'common.save' })}
        </Button>
      </div>
    </Card>
  );
}
