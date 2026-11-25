import { useIntl } from 'react-intl';
import type { ChannelConfigSettings } from '@/lib/api';
import { SettingsRow, Switch, Textarea } from '@/components/mds';

type SharedKey = 'shared_bot_binding' | 'unbound_reply';

export function GoogleChatSharedSettings({ role, settings, onChange }: {
  role: string | null | undefined;
  settings: Pick<ChannelConfigSettings, SharedKey>;
  onChange: <K extends SharedKey>(key: K, value: ChannelConfigSettings[K]) => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  if (role !== 'admin') return null;
  return <>
    <SettingsRow label={t('channels.googlechat.sharedBinding')} description={t('channels.googlechat.sharedBinding.help')}>
      <Switch
        aria-label={t('channels.googlechat.sharedBinding')}
        checked={settings.shared_bot_binding ?? false}
        onCheckedChange={(value) => onChange('shared_bot_binding', Boolean(value))}
      />
    </SettingsRow>
    <SettingsRow label={t('channels.googlechat.unboundReply')} description={t('channels.googlechat.unboundReply.help')}>
      <Textarea
        aria-label={t('channels.googlechat.unboundReply')}
        rows={4}
        value={settings.unbound_reply ?? ''}
        onChange={(event) => onChange('unbound_reply', event.target.value)}
      />
    </SettingsRow>
  </>;
}
