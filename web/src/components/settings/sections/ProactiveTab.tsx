import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { useAgentsStore } from '@/stores/agents-store';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
  SettingsSection,
  SettingsCard,
  SettingsSaveState,
} from '@/components/mds';
import { AdvancedSection, ScheduleBuilder, type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch, RowNumber, RowText, FieldBlock } from '@/pages/agent-form/form-rows';

// ── Proactive Settings Tab ─────────────────────────────────────

/**
 * W2-9 — client-side mirror of the gateway's `notify_governance::QuietWindow::
 * parse` (same validator duplicated in `agent-form/EditAgentPage.tsx` for
 * W2-8). Same `H:MM-H:MM`/`HH:MM-HH:MM` shape, same range check, same
 * degenerate-window (`start == end`) rejection. Deliberately narrower than
 * the backend's fail-open reader (which treats a malformed value as "no
 * quiet hours") — here an invalid-but-non-empty value must surface as a
 * visible inline error, not silently behave as if the field were blank.
 * Empty string is valid (means "not set").
 */
function isValidQuietHoursFormat(raw: string): boolean {
  const s = raw.trim();
  if (s === '') return true;
  const m = s.match(/^(\d{1,2}):(\d{2})-(\d{1,2}):(\d{2})$/);
  if (!m) return false;
  const h1 = Number(m[1]);
  const min1 = Number(m[2]);
  const h2 = Number(m[3]);
  const min2 = Number(m[4]);
  if (h1 > 23 || min1 > 59 || h2 > 23 || min2 > 59) return false;
  return !(h1 === h2 && min1 === min2);
}

/** `duduclaw_core::types::ProactiveConfig`'s own `Default` for the legacy
 *  numeric pair. Only treated as "the operator customized this" when the
 *  loaded value differs — every agent that never touched quiet hours reads
 *  back exactly this pair, and must not have a value spuriously introduced
 *  into the new governance field on that basis alone. */
const LEGACY_DEFAULT_START = 23;
const LEGACY_DEFAULT_END = 8;

function pad2(n: number): string {
  return n.toString().padStart(2, '0');
}

export function ProactiveTab() {
  const intl = useIntl();
  const { agents, fetchAgents } = useAgentsStore();
  const [selectedAgent, setSelectedAgent] = useState('');
  const [config, setConfig] = useState({
    enabled: false,
    check_interval: '*/30 * * * *',
    max_messages_per_hour: 3,
    notify_channel: '',
    notify_chat_id: '',
    notify_thread_id: '',
  });
  // W2-9 — the agent's OWN raw `[proactive] quiet_hours` string
  // (`HH:MM-HH:MM`), the ONLY field `notify_governance::load_agent_policy`
  // reads. The legacy `quiet_hours_start`/`quiet_hours_end` numeric pair
  // this page used to read/write is deliberately never sent again — the
  // gateway keeps it untouched on disk (still used by
  // `duduclaw-agent::proactive::is_quiet_hour` to gate the proactive
  // engine's OWN scheduled check-ins, a narrower and separate concern —
  // see `notify_governance.rs`'s module doc), but writing it from here no
  // longer pretends it also controls notification suppression.
  const [quietHours, setQuietHours] = useState('');
  // Non-null while the field was auto-prefilled from a customized legacy
  // pair the new governance layer could never see (the actual trap this
  // fix closes) — drives the migration hint below the field.
  const [quietHoursMigratedFrom, setQuietHoursMigratedFrom] = useState<string | null>(null);
  // The backend's zh-TW sentence stating exactly what's in effect right
  // now (`QuietPolicy::suppression_note_zh`), `null` when nothing is held
  // back.
  const [quietHoursNote, setQuietHoursNote] = useState<string | null>(null);
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
        max_messages_per_hour: detail?.proactive?.max_messages_per_hour ?? 3,
        notify_channel: detail?.proactive?.notify_channel ?? '',
        notify_chat_id: detail?.proactive?.notify_chat_id ?? '',
        notify_thread_id: detail?.proactive?.notify_thread_id ?? '',
      });

      // W2-9 — the agent's OWN new-format value (never the fallen-back
      // effective one; see `agent_raw_quiet_hours`'s doc for why prefilling
      // from the effective value would silently pin the global default into
      // this one agent on the next unrelated save).
      const ownQuietHours = detail?.proactive?.quiet_hours_own ?? '';
      const legacyStart = detail?.proactive?.quiet_hours_start ?? LEGACY_DEFAULT_START;
      const legacyEnd = detail?.proactive?.quiet_hours_end ?? LEGACY_DEFAULT_END;
      const legacyCustomised = legacyStart !== LEGACY_DEFAULT_START || legacyEnd !== LEGACY_DEFAULT_END;

      if (ownQuietHours) {
        setQuietHours(ownQuietHours);
        setQuietHoursMigratedFrom(null);
      } else if (legacyCustomised) {
        // The new governance field is unset, but the legacy numeric pair
        // carries a value that differs from its own default — evidence the
        // operator customized it via this very page before `quiet_hours`
        // (the string field) existed. `notify_governance` never reads the
        // legacy pair, so that customization has been silently invisible
        // to notification suppression ever since. Prefill the converted
        // value; the operator still has to press Save to persist it.
        const converted = `${pad2(legacyStart)}:00-${pad2(legacyEnd)}:00`;
        setQuietHours(converted);
        setQuietHoursMigratedFrom(converted);
      } else {
        setQuietHours('');
        setQuietHoursMigratedFrom(null);
      }
      setQuietHoursNote(detail?.proactive?.quiet_hours_note ?? null);
    }).catch((e) => {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [selectedAgent, intl]);

  const handleSave = async () => {
    if (!selectedAgent) return;
    setSaving(true);
    try {
      await api.agents.update(selectedAgent, {
        proactive: { ...config, quiet_hours: quietHours.trim() },
      });
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
          {/* W2-9 — quiet hours edits `[proactive] quiet_hours` (the
              `HH:MM-HH:MM` string), the only field `notify_governance`
              reads. The legacy numeric start/end pair this row used to
              read/write is no longer surfaced here: the gateway leaves it
              untouched on disk (it keeps its own, narrower job — see the
              state comment above), but this page must not pretend writing
              it also controls notification suppression. */}
          <RowText
            label={intl.formatMessage({ id: 'proactive.quietHours' })}
            description={intl.formatMessage({ id: 'proactive.quietHours.help' })}
            tier="select-wide"
            value={quietHours}
            placeholder="22:00-08:00"
            onChange={(v) => { setQuietHours(v); setQuietHoursMigratedFrom(null); }}
          />
          <RowSelect
            label={intl.formatMessage({ id: 'proactive.notifyChannel' })}
            description={intl.formatMessage({ id: 'proactive.notifyChannel.help' })}
            value={config.notify_channel}
            onChange={(v) => setConfig({ ...config, notify_channel: v })}
            options={channelOptions}
          />
        </SettingsCard>
        {quietHours.trim() !== '' && !isValidQuietHoursFormat(quietHours) && (
          <p className="px-1 text-xs text-destructive">
            {intl.formatMessage({ id: 'agents.adv.quietHours.invalid' })}
          </p>
        )}
        {quietHoursMigratedFrom && (
          <p className="px-1 text-xs text-muted-foreground">
            {intl.formatMessage({ id: 'proactive.quietHours.migrated' }, { value: quietHoursMigratedFrom })}
          </p>
        )}
        {quietHoursNote && (
          <p className="px-1 text-xs text-muted-foreground">{quietHoursNote}</p>
        )}

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
          <RowText
            label={intl.formatMessage({ id: 'agents.adv.notifyThreadId' })}
            description={intl.formatMessage({ id: 'agents.adv.notifyThreadId.help' })}
            value={config.notify_thread_id}
            onChange={(v) => setConfig({ ...config, notify_thread_id: v })}
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
