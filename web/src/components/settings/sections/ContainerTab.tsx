import { useIntl } from 'react-intl';
import { SettingsSection, SettingsCard } from '@/components/mds';
import { SettingRow } from './shared';

export function ContainerTab() {
  const intl = useIntl();

  return (
    <SettingsSection>
      <SettingsCard>
        <SettingRow label={intl.formatMessage({ id: 'settings.container.engine' })} value="Docker" />
        <SettingRow label={intl.formatMessage({ id: 'settings.container.socket' })} value="/var/run/docker.sock" />
        <SettingRow
          label={intl.formatMessage({ id: 'settings.container.status' })}
          value={intl.formatMessage({ id: 'settings.container.detected' })}
          badge="emerald"
        />
      </SettingsCard>
    </SettingsSection>
  );
}
