import { useIntl } from 'react-intl';
import { Card } from '@/components/ui';
import { Container } from 'lucide-react';
import { SettingRow } from './shared';

export function ContainerTab() {
  const intl = useIntl();

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <Container className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'settings.container' })}
        </span>
      }
    >
      <p className="mb-4 text-sm text-stone-500 dark:text-stone-400">
        {intl.formatMessage({ id: 'settings.container.desc' })}
      </p>
      <div className="space-y-4">
        <SettingRow label={intl.formatMessage({ id: 'settings.container.engine' })} value="Docker" />
        <SettingRow label={intl.formatMessage({ id: 'settings.container.socket' })} value="/var/run/docker.sock" />
        <SettingRow
          label={intl.formatMessage({ id: 'settings.container.status' })}
          value={intl.formatMessage({ id: 'settings.container.detected' })}
          badge="emerald"
        />
      </div>
    </Card>
  );
}
