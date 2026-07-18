import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Input,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
} from '@/components/mds';
import { AdvancedSection, ScheduleBuilder, type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch, RowNumber, RowText, FieldBlock } from '@/pages/agent-form/form-rows';

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

  const currentAgent = agents.find((a) => a.name === selectedAgent);

  return (
    <div className="space-y-8">
      {/* Per-agent scope picker */}
      <div className="flex justify-end">
        <Select value={selectedAgent} onValueChange={(v) => setSelectedAgent(String(v))} disabled={agents.length === 0}>
          <SelectTrigger size="sm" aria-label={intl.formatMessage({ id: 'proactive.title' })}>
            <SelectValue>{currentAgent ? (currentAgent.display_name || currentAgent.name) : intl.formatMessage({ id: 'common.noData' })}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            {agents.map((a) => (
              <SelectItem key={a.name} value={a.name}>{a.display_name || a.name}</SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <SettingsSection>
        <SettingsCard>
          <RowSwitch
            label={intl.formatMessage({ id: config.enabled ? 'proactive.enabled' : 'proactive.disabled' })}
            description={intl.formatMessage({ id: 'proactive.enabled.help' })}
            checked={config.enabled}
            onChange={(v) => setConfig({ ...config, enabled: v })}
          />
          <RowNumber
            label={intl.formatMessage({ id: 'proactive.maxMessagesPerHour' })}
            description={intl.formatMessage({ id: 'proactive.maxMessagesPerHour.help' })}
            value={config.max_messages_per_hour}
            min={1}
            max={60}
            onChange={(v) => setConfig({ ...config, max_messages_per_hour: v })}
          />
          <SettingsRow
            label={intl.formatMessage({ id: 'proactive.quietHours' })}
            description={intl.formatMessage({ id: 'proactive.quietHours.help' })}
            tier="select-wide"
          >
            <div className="flex items-center gap-2">
              <Input type="number" min={0} max={23} value={config.quiet_hours_start}
                onChange={(e) => setConfig({ ...config, quiet_hours_start: +e.target.value })}
                className="w-16 text-center" />
              <span className="text-muted-foreground">—</span>
              <Input type="number" min={0} max={23} value={config.quiet_hours_end}
                onChange={(e) => setConfig({ ...config, quiet_hours_end: +e.target.value })}
                className="w-16 text-center" />
            </div>
          </SettingsRow>
          <RowSelect
            label={intl.formatMessage({ id: 'proactive.notifyChannel' })}
            description={intl.formatMessage({ id: 'proactive.notifyChannel.help' })}
            value={config.notify_channel}
            onChange={(v) => setConfig({ ...config, notify_channel: v })}
            options={channelOptions}
          />
        </SettingsCard>

        <FieldBlock
          label={intl.formatMessage({ id: 'proactive.checkInterval' })}
          description={intl.formatMessage({ id: 'proactive.checkInterval.help2' })}
        >
          <ScheduleBuilder
            value={config.check_interval}
            onChange={(cron) => setConfig({ ...config, check_interval: cron })}
          />
        </FieldBlock>
      </SettingsSection>

      <AdvancedSection storageKey="settings.proactive" label={intl.formatMessage({ id: 'proactive.advanced' })}>
        <SettingsCard>
          <RowText
            label={intl.formatMessage({ id: 'proactive.chatId' })}
            description={intl.formatMessage({ id: 'proactive.chatId.help' })}
            value={config.notify_chat_id}
            onChange={(v) => setConfig({ ...config, notify_chat_id: v })}
            placeholder="e.g., 123456789"
          />
        </SettingsCard>
      </AdvancedSection>

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
