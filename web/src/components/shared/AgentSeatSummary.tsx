import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type LicenseSnapshot } from '@/lib/api';

/** Counts come from the authoritative seat scan, never from the visible roster. */
export function AgentSeatSummary({ refreshKey }: { refreshKey?: unknown }) {
  const intl = useIntl();
  const [status, setStatus] = useState<LicenseSnapshot | null>(null);
  useEffect(() => {
    let active = true;
    setStatus(null);
    void api.license.status().then(value => { if (active) setStatus(value); })
      .catch(() => { if (active) setStatus(null); });
    return () => { active = false; };
  }, [refreshKey]);
  const counts = status && typeof status.seats_used === 'number'
    && typeof status.avatars_exempt === 'number'
    && (typeof status.seat_limit === 'number' || status.seat_unlimited === true) ? status : null;
  return <div className="space-y-1 px-4 py-3 text-sm text-muted-foreground">
    {counts && <p data-testid="agent-seat-counts">{intl.formatMessage({ id: 'agents.seats.summary' }, {
      used: counts.seats_used, avatars: counts.avatars_exempt,
      limit: counts.seat_unlimited ? intl.formatMessage({ id: 'license.seats.unlimited' }) : counts.seat_limit,
    })}</p>}
    <p>{intl.formatMessage({ id: 'agents.seats.avatarExempt' })}</p>
    <p className="text-xs">{intl.formatMessage({ id: 'agents.seats.avatarConditions' })}</p>
  </div>;
}
