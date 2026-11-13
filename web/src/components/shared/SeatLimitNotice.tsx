import { useIntl } from 'react-intl';
import { seatStatusKey, type SeatLimitInfo } from '@/lib/seat-limit';

/**
 * SeatLimitNotice — why creating (or restoring) an AI employee was refused
 * by the licence seat limit (RM-149): the gateway's message, then every
 * employee currently counted (id, display name, status) so an admin can see
 * which ones to delete. Archived employees still hold a seat; deleted ones
 * do not and never appear here.
 */
export function SeatLimitNotice({ info }: { info: SeatLimitInfo }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const statusLabel = (status: string) => {
    const key = seatStatusKey(status);
    return key ? t(key) : status;
  };
  return (
    <div role="alert" className="space-y-2 rounded-lg border border-destructive/30 p-3 text-sm">
      <p className="font-medium text-destructive">{t('agents.seatLimit.title')}</p>
      {info.message && <p className="whitespace-pre-wrap break-words text-foreground">{info.message}</p>}
      {info.max !== null && info.current !== null && (
        <p className="text-muted-foreground">{t('agents.seatLimit.summary', { max: info.max, current: info.current })}</p>
      )}
      <p className="text-muted-foreground">{t('agents.seatLimit.hint')}</p>
      {info.holders.length > 0 && (
        <div>
          <p className="text-xs font-medium text-muted-foreground">{t('agents.seatLimit.listLabel')}</p>
          <ul aria-label={t('agents.seatLimit.listLabel')} className="mt-1 space-y-1">
            {info.holders.map((h) => (
              <li key={h.id} className="flex flex-wrap items-baseline gap-x-2">
                <span className="font-mono text-xs">{h.id}</span>
                {h.display_name !== h.id && <span>{h.display_name}</span>}
                <span className="text-xs text-muted-foreground">{statusLabel(h.status)}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
