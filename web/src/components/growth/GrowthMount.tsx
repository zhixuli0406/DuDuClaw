import { useEffect } from 'react';
import { useIntl } from 'react-intl';
import { useConnectionStore } from '@/stores/connection-store';
import { useSharedLeaderQuery } from '@/hooks/useSharedLeaderQuery';
import { useGrowthStore, growthEventBus } from '@/stores/growth-store';
import { growthApi, type GrowthSnapshot } from '@/lib/api-growth';
import { toast } from '@/lib/toast';
import { ACHIEVEMENT_DEFS } from './achievements-def';
import { DailyReportCard } from './DailyReportCard';

/**
 * GrowthMount — the single, always-mounted driver for the gamification layer
 * (V10). Mounted once in `MainLayout` so:
 *   1. exactly ONE `growth.snapshot` poll runs (multi-tab shared via the leader
 *      hook), feeding the store, whose diff fires unlock/level-up moments once.
 *   2. the store's `growthEventBus` events become localized toasts here (this
 *      works in reduced-motion too, where the confetti burst is suppressed —
 *      the toast is then the only acknowledgement). The bus is also the hook
 *      point for DuDu's transient `proud` face (§7.2 / W4b).
 *   3. the once-per-day settlement dialog (`DailyReportCard`) is hosted.
 *
 * Polling only runs while authenticated; nothing here renders visible chrome
 * except the (usually closed) dialog.
 */
export function GrowthMount() {
  const intl = useIntl();
  const authed = useConnectionStore((s) => s.state === 'authenticated');
  const applySnapshot = useGrowthStore((s) => s.applySnapshot);

  const { data } = useSharedLeaderQuery<GrowthSnapshot>(
    'growth.snapshot',
    () => growthApi.snapshot(),
    60_000,
    authed,
  );

  useEffect(() => {
    if (data) applySnapshot(data);
  }, [data, applySnapshot]);

  // Turn store transitions into localized acknowledgements.
  useEffect(
    () =>
      growthEventBus.subscribe((ev) => {
        if (ev.type === 'achievement_unlocked') {
          const def = ACHIEVEMENT_DEFS[ev.id];
          const name = def ? intl.formatMessage({ id: def.nameId }) : ev.id;
          toast.success(intl.formatMessage({ id: 'growth.toast.unlocked' }, { name }));
        } else if (ev.type === 'level_up') {
          toast.success(intl.formatMessage({ id: 'growth.toast.levelUp' }, { level: ev.level }));
        }
      }),
    [intl],
  );

  return <DailyReportCard />;
}
