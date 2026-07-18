import { useEffect, useState, useRef } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Input,
  SettingsSection,
  SettingsCard,
  SettingsSaveState,
} from '@/components/mds';
import { AdvancedSection, type SelectOption } from '@/components/settings/controls';
import { RowSelect, RowSwitch, RowText, FieldBlock } from '@/pages/agent-form/form-rows';

// ── Voice Settings Tab ─────────────────────────────────────────

/** STT config shape mirrored from `GET/POST /api/voice/config`. */
interface SttConfig {
  stt_provider: string;
  stt_base_url: string;
  stt_model: string;
  stt_command: string;
}

const EMPTY_STT: SttConfig = {
  stt_provider: '',
  stt_base_url: '',
  stt_model: '',
  stt_command: '',
};

export function VoiceTab() {
  const intl = useIntl();
  const [config, setConfig] = useState({
    asr_provider: 'auto',
    tts_provider: 'auto',
    asr_language: 'zh',
    tts_voice: '',
    voice_reply_enabled: false,
  });
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  // Load persisted [voice] settings from inference.toml on mount.
  useEffect(() => {
    api.system.config().then((res) => {
      if (res?.voice) {
        setConfig((prev) => ({ ...prev, ...res.voice }));
      }
    }).catch((e) => {
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.system.updateConfig({ voice: config });
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const asrOptions: SelectOption[] = [
    { value: 'auto', label: intl.formatMessage({ id: 'voice.provider.auto' }), raw: 'auto' },
    { value: 'whisper-api', label: intl.formatMessage({ id: 'voice.provider.whisperApi' }), raw: 'whisper-api' },
    { value: 'whisper-local', label: 'Whisper Local', raw: 'whisper-local' },
  ];
  const ttsOptions: SelectOption[] = [
    { value: 'auto', label: intl.formatMessage({ id: 'voice.provider.auto' }), raw: 'auto' },
    { value: 'edge-tts', label: intl.formatMessage({ id: 'voice.provider.edgeTts' }), raw: 'edge-tts' },
    { value: 'minimax', label: intl.formatMessage({ id: 'voice.provider.minimax' }), raw: 'minimax' },
    { value: 'openai-tts', label: intl.formatMessage({ id: 'voice.provider.openaiTts' }), raw: 'openai-tts' },
    { value: 'piper', label: intl.formatMessage({ id: 'voice.provider.piper' }), raw: 'piper' },
  ];
  const langOptions: SelectOption[] = [
    { value: 'zh', label: '中文', raw: 'zh' },
    { value: 'en', label: 'English', raw: 'en' },
    { value: 'ja', label: '日本語', raw: 'ja' },
    { value: 'ko', label: '한국어', raw: 'ko' },
  ];

  return (
    <div className="space-y-8">
      <SettingsSection>
        <SettingsCard>
          <RowSwitch
            label={intl.formatMessage({ id: 'voice.voiceMode' })}
            description={intl.formatMessage({ id: 'voice.voiceMode.help' })}
            checked={config.voice_reply_enabled}
            onChange={(v) => setConfig({ ...config, voice_reply_enabled: v })}
          />
          <RowSelect
            label={intl.formatMessage({ id: 'voice.asrProvider' })}
            description={intl.formatMessage({ id: 'voice.asrProvider.help' })}
            value={config.asr_provider}
            onChange={(v) => setConfig({ ...config, asr_provider: v })}
            options={asrOptions}
          />
          <RowSelect
            label={intl.formatMessage({ id: 'voice.ttsProvider' })}
            description={intl.formatMessage({ id: 'voice.ttsProvider.help' })}
            value={config.tts_provider}
            onChange={(v) => setConfig({ ...config, tts_provider: v })}
            options={ttsOptions}
          />
          <RowSelect
            label={intl.formatMessage({ id: 'voice.language' })}
            description={intl.formatMessage({ id: 'voice.language.help' })}
            value={config.asr_language}
            onChange={(v) => setConfig({ ...config, asr_language: v })}
            options={langOptions}
          />
        </SettingsCard>
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
      </SettingsSection>

      <AdvancedSection storageKey="settings.voice" label={intl.formatMessage({ id: 'voice.advanced' })}>
        <SttConfigCard />
      </AdvancedSection>
    </div>
  );
}

/**
 * Speech-to-text provider chain (openhuman-parity B-P1). Persisted separately to
 * `config.toml [voice]` via `POST /api/voice/config` (the API key is encrypted
 * at rest). When unset, the `/api/stt` endpoint fails closed with a 501.
 */
function SttConfigCard() {
  const intl = useIntl();
  const [stt, setStt] = useState<SttConfig>(EMPTY_STT);
  const [apiKey, setApiKey] = useState('');
  const [keySet, setKeySet] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (savedTimerRef.current) clearTimeout(savedTimerRef.current); }, []);

  useEffect(() => {
    const jwt = useAuthStore.getState().jwt;
    fetch('/api/voice/config', { headers: jwt ? { Authorization: `Bearer ${jwt}` } : {} })
      .then((r) => (r.ok ? r.json() : null))
      .then((data) => {
        if (!data) return;
        setStt({
          stt_provider: data.stt_provider ?? '',
          stt_base_url: data.stt_base_url ?? '',
          stt_model: data.stt_model ?? '',
          stt_command: data.stt_command ?? '',
        });
        setKeySet(!!data.stt_api_key_set);
      })
      .catch(() => { /* first-run / unauthorized — leave defaults */ });
  }, []);

  const handleSave = async () => {
    setSaving(true);
    try {
      const jwt = useAuthStore.getState().jwt;
      const payload: Record<string, unknown> = { ...stt };
      // Only send the key when the user typed a new one (empty → keep existing).
      if (apiKey) payload.stt_api_key = apiKey;
      const res = await fetch('/api/voice/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...(jwt ? { Authorization: `Bearer ${jwt}` } : {}) },
        body: JSON.stringify(payload),
      });
      if (!res.ok) {
        const body = await res.json().catch(() => null);
        throw new Error(body?.error ?? `HTTP ${res.status}`);
      }
      if (apiKey) { setKeySet(true); setApiKey(''); }
      setSaved(true);
      savedTimerRef.current = setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setSaving(false);
    }
  };

  const isOpenAi = stt.stt_provider === 'openai_compat';
  const isCommand = stt.stt_provider === 'command';

  const providerOptions: SelectOption[] = [
    { value: '', label: intl.formatMessage({ id: 'voice.stt.providerNone', defaultMessage: '未設定（停用語音輸入）' }), raw: '' },
    { value: 'openai_compat', label: intl.formatMessage({ id: 'voice.stt.providerOpenai', defaultMessage: 'OpenAI 相容（Whisper API / Groq）' }), raw: 'openai_compat' },
    { value: 'command', label: intl.formatMessage({ id: 'voice.stt.providerCommand', defaultMessage: '本地指令（whisper-cli 等）' }), raw: 'command' },
  ];

  return (
    <SettingsSection
      title={intl.formatMessage({ id: 'voice.stt.title', defaultMessage: '語音轉文字（STT）' })}
      description={intl.formatMessage({
        id: 'voice.stt.desc',
        defaultMessage: '設定聊天頁「按住說話」的語音辨識來源。未設定時語音輸入會停用。',
      })}
    >
      <SettingsCard>
        <RowSelect
          label={intl.formatMessage({ id: 'voice.stt.provider', defaultMessage: 'STT 供應商' })}
          value={stt.stt_provider}
          onChange={(v) => setStt({ ...stt, stt_provider: v })}
          options={providerOptions}
        />
      </SettingsCard>

      {isOpenAi && (
        <SettingsCard>
          <RowText
            label={intl.formatMessage({ id: 'voice.stt.baseUrl', defaultMessage: 'API Base URL' })}
            value={stt.stt_base_url}
            onChange={(v) => setStt({ ...stt, stt_base_url: v })}
            placeholder="https://api.openai.com/v1"
          />
          <RowText
            label={intl.formatMessage({ id: 'voice.stt.model', defaultMessage: '模型' })}
            value={stt.stt_model}
            onChange={(v) => setStt({ ...stt, stt_model: v })}
            placeholder="whisper-1"
          />
          <RowText
            label={intl.formatMessage({ id: 'voice.stt.apiKey', defaultMessage: 'API Key' })}
            type="password"
            value={apiKey}
            onChange={setApiKey}
            placeholder={keySet
              ? intl.formatMessage({ id: 'voice.stt.apiKeySet', defaultMessage: '已設定（留空以保留）' })
              : intl.formatMessage({ id: 'voice.stt.apiKeyPlaceholder', defaultMessage: 'sk-…' })}
          />
        </SettingsCard>
      )}

      {isCommand && (
        <FieldBlock
          label={intl.formatMessage({ id: 'voice.stt.command', defaultMessage: '轉錄指令' })}
          description={intl.formatMessage({
            id: 'voice.stt.commandHint',
            defaultMessage: '{audio} 會被替換成暫存音檔路徑；轉錄結果讀自指令的標準輸出。',
          })}
        >
          <Input
            type="text"
            value={stt.stt_command}
            onChange={(e) => setStt({ ...stt, stt_command: e.target.value })}
            placeholder="whisper-cli -m /models/ggml-base.bin -f {audio} --output-txt --no-prints"
          />
        </FieldBlock>
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
    </SettingsSection>
  );
}
