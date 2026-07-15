import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type ChannelStatus } from '@/lib/api';
import { Card, Badge } from '@/components/ui';
import { Radio } from 'lucide-react';

/**
 * ChannelHealthCard — the admin-only 通道健康 home widget (WP15). A thin
 * read-only summary over `channels.status`; management lives in /manage/channels.
 */
export function ChannelHealthCard({ enabled }: { enabled: boolean }) {
  const intl = useIntl();
  const [channels, setChannels] = useState<ChannelStatus[] | null>(null);

  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    api.channels
      .status()
      .then((r) => alive && setChannels(r.channels ?? []))
      .catch(() => alive && setChannels([]));
    return () => {
      alive = false;
    };
  }, [enabled]);

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <Radio className="h-4 w-4 text-amber-500" />
          {intl.formatMessage({ id: 'home.widget.channel_health' })}
        </span>
      }
    >
      {channels === null ? (
        <p className="py-4 text-center text-sm text-stone-400">{intl.formatMessage({ id: 'common.loading' })}</p>
      ) : channels.length === 0 ? (
        <p className="py-4 text-center text-sm text-stone-400">
          {intl.formatMessage({ id: 'home.widget.channel_health.empty' })}
        </p>
      ) : (
        <ul className="space-y-1.5">
          {channels.map((c) => (
            <li key={c.name} className="flex items-center justify-between text-sm">
              <span className="text-stone-700 dark:text-stone-300">{c.name}</span>
              <Badge tone={c.connected ? 'success' : 'neutral'} dot>
                {intl.formatMessage({ id: c.connected ? 'home.widget.channel.on' : 'home.widget.channel.off' })}
              </Badge>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}
