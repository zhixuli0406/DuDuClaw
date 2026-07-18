import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Radio } from 'lucide-react';
import { api, type ChannelStatus } from '@/lib/api';
import { cn } from '@/lib/utils';
import { Card, CardHeader, CardTitle } from '@/components/mds';

/**
 * ChannelHealthCard — the admin-only 通道健康 home widget (WP1.5). A thin
 * read-only summary over `channels.status` as a status-dot list; management
 * lives in `/manage/channels`.
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
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Radio className="size-4 text-brand" />
          {intl.formatMessage({ id: 'home.widget.channel_health' })}
        </CardTitle>
      </CardHeader>
      <div className="px-4">
        {channels === null ? (
          <p className="py-2 text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'common.loading' })}
          </p>
        ) : channels.length === 0 ? (
          <p className="py-2 text-sm text-muted-foreground">
            {intl.formatMessage({ id: 'home.widget.channel_health.empty' })}
          </p>
        ) : (
          <ul className="space-y-1">
            {channels.map((c) => (
              <li key={c.name} className="flex h-8 items-center justify-between text-sm">
                <span className="text-foreground">{c.name}</span>
                <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                  <span
                    aria-hidden
                    className={cn(
                      'size-1.5 rounded-full',
                      c.connected ? 'bg-success' : 'bg-muted-foreground/50',
                    )}
                  />
                  {intl.formatMessage({
                    id: c.connected ? 'home.widget.channel.on' : 'home.widget.channel.off',
                  })}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Card>
  );
}
