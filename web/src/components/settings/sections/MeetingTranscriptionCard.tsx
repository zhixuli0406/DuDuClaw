import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { Button, ErrorState, Input, SettingsCard, SettingsRow, SettingsSection } from '@/components/mds';
import { RowSelect, RowSwitch, RowText } from '@/pages/agent-form/form-rows';

type GoogleSttConfig = {
  project_id: string; location: string; bucket: string;
  language: string; model: string; diarization: boolean;
};
type Config = {
  engine: 'gemini' | 'google_stt' | 'whisper'; model: string | null; max_bytes: number;
  google_stt?: GoogleSttConfig;
};
const defaultGoogleStt: GoogleSttConfig = {
  project_id: '', location: 'asia-southeast1', bucket: '',
  language: 'cmn-Hant-TW', model: 'chirp_2', diarization: false,
};

export function MeetingTranscriptionCard() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const isAdmin = useAuthStore((state) => state.user?.role === 'admin');
  const [config, setConfig] = useState<Config | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [credentialCleared, setCredentialCleared] = useState(false);
  const [googleKey, setGoogleKey] = useState('');
  const load = useCallback(async () => {
    setError(null);
    setConfig(null);
    setSaved(false);
    setCredentialCleared(false);
    setGoogleKey('');
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
  const google = config?.google_stt ?? defaultGoogleStt;
  const updateGoogle = (fields: Partial<GoogleSttConfig>) => {
    if (!config) return;
    setSaved(false);
    setCredentialCleared(false);
    setConfig({ ...config, google_stt: { ...google, ...fields } });
  };
  const save = async () => {
    if (!config) return;
    setSaving(true);
    setError(null);
    setSaved(false);
    setCredentialCleared(false);
    try {
      await api.system.updateConfig({
        transcription: config.engine === 'google_stt' ? { ...config, google_stt: google } : config,
      });
      if (config.engine === 'google_stt' && googleKey.trim()) {
        await api.system.setTranscriptionCredential(googleKey);
      }
      setGoogleKey('');
      setSaved(true);
    } catch (failure) { setError(failure); }
    finally { setSaving(false); }
  };
  const clearCredential = async () => {
    setSaving(true);
    setError(null);
    setSaved(false);
    setCredentialCleared(false);
    try {
      await api.system.clearTranscriptionCredential();
      setGoogleKey('');
      setCredentialCleared(true);
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
            { value: 'google_stt', label: t('meeting.settings.google'), raw: 'google_stt' },
          ]}
          onChange={(engine) => { setSaved(false); setCredentialCleared(false); setGoogleKey(''); setConfig({ ...config, engine: engine as Config['engine'] }); }}
        />
        {config.engine === 'gemini' && <RowText
          label={t('meeting.settings.model')}
          description={t('meeting.settings.modelHint')}
          value={config.model ?? ''}
          onChange={(model) => { setSaved(false); setConfig({ ...config, model: model || null }); }}
        />}
        {config.engine === 'google_stt' && <>
          <p className="px-4 py-3 text-sm text-muted-foreground">{t('meeting.settings.googleHint')}</p>
          {(['project_id', 'bucket'] as const).map((field) => (
            <SettingsRow key={field} label={t(`meeting.settings.google.${field}`)} tier="text">
              <Input
                aria-label={t(`meeting.settings.google.${field}`)}
                value={google[field]}
                onChange={(event) => updateGoogle({ [field]: event.target.value })}
              />
            </SettingsRow>
          ))}
          <RowSelect
            label={t('meeting.settings.google.location')}
            value={google.location}
            options={['asia-southeast1', 'us-central1', 'europe-west4'].map((location) => ({ value: location, label: location, raw: location }))}
            onChange={(location) => updateGoogle({ location })}
          />
          <RowSelect
            label={t('meeting.settings.google.language')}
            value={google.language}
            options={[{ value: 'cmn-Hant-TW', label: 'cmn-Hant-TW', raw: 'cmn-Hant-TW' }]}
            onChange={(language) => updateGoogle({ language })}
          />
          <RowSelect
            label={t('meeting.settings.google.model')}
            value={google.model}
            options={[{ value: 'chirp_2', label: 'Chirp 2', raw: 'chirp_2' }]}
            onChange={(model) => updateGoogle({ model })}
          />
          <RowSwitch
            label={t('meeting.settings.google.diarization')}
            description={t('meeting.settings.google.diarizationHint')}
            checked={false}
            disabled
            onChange={() => {}}
          />
          <SettingsRow label={t('meeting.settings.google.key')} description={t('meeting.settings.google.keyHint')} tier="text">
            <Input
              aria-label={t('meeting.settings.google.key')}
              type="password"
              autoComplete="new-password"
              value={googleKey}
              onChange={(event) => { setSaved(false); setCredentialCleared(false); setGoogleKey(event.target.value); }}
            />
          </SettingsRow>
          <SettingsRow label={t('meeting.settings.google.clearKey')} description={t('meeting.settings.google.clearKeyHint')} tier="text">
            <Button size="sm" disabled={saving} onClick={() => void clearCredential()}>
              {t('meeting.settings.google.clearKey')}
            </Button>
          </SettingsRow>
        </>}
      </SettingsCard>}
      <p className="text-sm text-muted-foreground">{t('meeting.settings.requirements')}</p>
      <div className="flex justify-end gap-3 items-center">
        {credentialCleared && <span role="status" className="text-sm">{t('meeting.settings.google.keyCleared')}</span>}
        {saved && <span role="status" className="text-sm">{t('settings.general.saved')}</span>}
        <Button variant="brand" size="sm" disabled={!config || saving} onClick={() => void save()}>
          {saving ? t('common.saving') : t('common.save')}
        </Button>
      </div>
    </SettingsSection>
  );
}
