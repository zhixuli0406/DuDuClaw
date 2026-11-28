import { useIntl } from 'react-intl';
import { Badge } from '@/components/mds';

export function CreatorAccessLostBadge() {
  const intl = useIntl();
  return <Badge variant="secondary" className="shrink-0 bg-warning/15 text-warning" title={intl.formatMessage({ id: 'routines.creatorAccessLost.hint' })}>
    {intl.formatMessage({ id: 'autopilot.provenance.accessLost' })}
  </Badge>;
}
