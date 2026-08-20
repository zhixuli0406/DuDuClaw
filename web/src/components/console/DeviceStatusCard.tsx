import type { ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Activity, Cpu, Gauge, MemoryStick, HardDrive, Thermometer, Timer, Wifi } from 'lucide-react';
import type { DeviceStatusArtifact } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';
import { formatMb, formatUptime } from './format';

/** One compact label/value row — the conversational, single-line sibling of
 *  DevicePage's bordered `StatTile` grid (this card is meant to sit inline
 *  in a ~28rem-wide chat card, not a settings page). */
function StatRow({
  icon: Icon,
  label,
  value,
}: {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-3 text-sm">
      <span className="flex min-w-0 items-center gap-1.5 text-muted-foreground">
        <Icon className="size-3.5 shrink-0" aria-hidden="true" />
        <span className="truncate">{label}</span>
      </span>
      <span className="shrink-0 font-medium tabular-nums text-foreground">{value}</span>
    </div>
  );
}

/** Compact usage bar shared by the RAM/磁碟 rows — mirrors DevicePage's. */
function UsageBar({ usedMb, totalMb }: { usedMb: number; totalMb: number }) {
  const pct = totalMb > 0 ? Math.min(100, Math.round((usedMb / totalMb) * 100)) : 0;
  return (
    <div className="h-1 w-16 shrink-0 overflow-hidden rounded-full bg-muted">
      <div className="h-full rounded-full bg-brand transition-all duration-700" style={{ width: `${pct}%` }} />
    </div>
  );
}

/**
 * 裝置狀態卡 — the conversational twin of DevicePage's ①狀態卡 (WP-C). Renders
 * one `device.status` snapshot; unlike DevicePage this card never polls —
 * it's a one-shot result the agent already fetched, so a stale reading is
 * refreshed by asking again ("再查一次裝置狀態"), not a background timer.
 * A `null` sensor field is omitted (an honest gap), never shown as 0.
 */
export function DeviceStatusCard({ payload }: { payload: DeviceStatusArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const upInterfaces = payload.network_interfaces.filter((i) => i.is_up).length;
  const totalInterfaces = payload.network_interfaces.length;

  return (
    <ArtifactShell
      icon={Activity}
      title={t('console.artifact.deviceStatus.title')}
      advancedTo="/device"
    >
      <div className="space-y-2">
        <StatRow icon={Cpu} label={t('device.status.cpuCores')} value={payload.cpu_cores} />
        {payload.load_average && (
          <StatRow
            icon={Gauge}
            label={t('device.status.load')}
            value={`${payload.load_average.load1.toFixed(2)} / ${payload.load_average.load5.toFixed(2)} / ${payload.load_average.load15.toFixed(2)}`}
          />
        )}
        {payload.ram && (
          <StatRow
            icon={MemoryStick}
            label={t('device.status.ram')}
            value={
              <span className="flex items-center gap-2">
                {formatMb(payload.ram.used_mb)} / {formatMb(payload.ram.total_mb)}
                <UsageBar usedMb={payload.ram.used_mb} totalMb={payload.ram.total_mb} />
              </span>
            }
          />
        )}
        {payload.disk && (
          <StatRow
            icon={HardDrive}
            label={t('device.status.disk')}
            value={
              <span className="flex items-center gap-2">
                {formatMb(payload.disk.used_mb)} / {formatMb(payload.disk.total_mb)}
                <UsageBar usedMb={payload.disk.used_mb} totalMb={payload.disk.total_mb} />
              </span>
            }
          />
        )}
        {payload.temperature_c != null && (
          <StatRow icon={Thermometer} label={t('device.status.temperature')} value={`${payload.temperature_c.toFixed(1)} °C`} />
        )}
        {payload.uptime_secs != null && (
          <StatRow icon={Timer} label={t('device.status.uptime')} value={formatUptime(payload.uptime_secs)} />
        )}
        {totalInterfaces > 0 && (
          <StatRow
            icon={Wifi}
            label={t('device.status.network')}
            value={t('device.status.network.summary', { up: upInterfaces, total: totalInterfaces })}
          />
        )}
      </div>
    </ArtifactShell>
  );
}
