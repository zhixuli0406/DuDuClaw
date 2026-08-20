import { useIntl } from 'react-intl';
import { Wifi } from 'lucide-react';
import { Badge } from '@/components/mds';
import type { NetworkInfoArtifact } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';

/** 網路資訊卡 — the conversational twin of DevicePage's ③網路卡 (read-only
 *  interface list; setting a static IP isn't implemented server-side yet,
 *  same limitation as DevicePage). */
export function NetworkInfoCard({ payload }: { payload: NetworkInfoArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  return (
    <ArtifactShell icon={Wifi} title={t('device.section.network')} advancedTo="/device">
      {payload.interfaces.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t('device.network.empty')}</p>
      ) : (
        <div className="space-y-1.5">
          {payload.interfaces.map((iface) => (
            <div
              key={iface.name}
              className="flex items-center justify-between gap-2 rounded-lg border border-surface-border px-2.5 py-1.5"
            >
              <span className="flex min-w-0 items-center gap-2">
                <span className="font-mono text-xs text-foreground">{iface.name}</span>
                <Badge variant="secondary" className={iface.is_up ? 'bg-success/15 text-success' : undefined}>
                  {t(iface.is_up ? 'device.network.status.up' : 'device.network.status.down')}
                </Badge>
              </span>
              <span className="truncate font-mono text-xs text-muted-foreground">
                {iface.addresses.length > 0 ? iface.addresses.join(', ') : '—'}
              </span>
            </div>
          ))}
        </div>
      )}
    </ArtifactShell>
  );
}
