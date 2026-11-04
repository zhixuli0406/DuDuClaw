import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { Button, ErrorState, SettingsCard, SettingsSection } from '@/components/mds';
import { RowSelect, RowText } from '@/pages/agent-form/form-rows';

type Config = { engine: 'gemini' | 'google_stt' | 'whisper'; model: string | null; max_bytes: number };

export function MeetingTranscriptionCard() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const isAdmin = useAuthStore((state) => state.user?.role === 'admin');
  const [config, setConfig] = useState<Config | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const load = useCallback(async () => {
    setError(null);
    setConfig(null);
    try {
      const response = await api.system.config();
      if (response.transcription_error || !response.transcription) {
        throw new Error(response.transcription_error || intl.formatMessage({ id: 'meeting.settings.error' }));
      }
      setConfig(response.transcription);
    } catch (failure) { setError(failure); }
  }, [intl]);
  useEffect(() => { if (isAdmin) void load(); }, [isAdmin, load]);
  if (!isAdmin) return null;
  const save = async () => {
    if (!config) return;
    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      await api.system.updateConfig({ transcription: config });
      setSaved(true);
    } catch (failure) { setError(failure); }
    finally { setSaving(false); }
  };
  return (
    <SettingsSection title={t('meeting.settings.title')} description={t('meeting.settings.privacy')}>
      {error != null && <ErrorState variant="inline" error={error} title={t('meeting.settings.error')} onRetry={() => void load()} />}
      {config && <SettingsCard>
        <RowSelect
          label={t('meeting.settings.engine')}
          value={config.engine}
          options={[
            { value: 'gemini', label: 'Gemini', raw: 'gemini' },
            { value: 'whisper', label: 'OpenAI Whisper', raw: 'whisper' },
            { value: 'google_stt', label: t('meeting.settings.googleUnsupported'), raw: 'google_stt' },
          ]}
          onChange={(engine) => { setSaved(false); setConfig({ ...config, engine: engine as Config['engine'] }); }}
        />
        {config.engine === 'gemini' && <RowText
          label={t('meeting.settings.model')}
          description={t('meeting.settings.modelHint')}
          value={config.model ?? ''}
          onChange={(model) => { setSaved(false); setConfig({ ...config, model: model || null }); }}
        />}
      </SettingsCard>}
      <p className="text-sm text-muted-foreground">{t('meeting.settings.requirements')}</p>
      <div className="flex justify-end gap-3 items-center">
        {saved && <span role="status" className="text-sm">{t('settings.general.saved')}</span>}
        <Button variant="brand" size="sm" disabled={!config || saving} onClick={() => void save()}>
          {saving ? t('common.saving') : t('common.save')}
        </Button>
      </div>
    </SettingsSection>
  );
}
