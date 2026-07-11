import { useIntl } from 'react-intl';
import { HelpCircle } from 'lucide-react';
import { AchievementBadge } from '@/components/ui';
import type { Achievement } from '@/lib/api-growth';
import { ACHIEVEMENT_DEFS } from './achievements-def';

/** Format an RFC3339 timestamp to a compact `YYYY-MM-DD`. Invalid ⇒ undefined. */
function fmtDate(iso?: string | null): string | undefined {
  if (!iso) return undefined;
  const t = new Date(iso);
  if (!Number.isFinite(t.getTime())) return undefined;
  const y = t.getFullYear();
  const m = String(t.getMonth() + 1).padStart(2, '0');
  const d = String(t.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

/**
 * One cell on the `/growth` achievement wall (§6.3). Three visual states:
 *  - unlocked  → glowing AchievementBadge with the unlock date.
 *  - locked    → desaturated AchievementBadge with a progress bar.
 *  - unavailable (`available === false`) → an explicit "暫不可用" chip with the
 *    backend's reason in a tooltip. This is NOT a 0-progress lock — the gateway
 *    simply cannot evaluate this one yet, and pretending it's at 0% would be a
 *    lie (§6.3 honesty rule).
 */
export function AchievementCell({ ach }: { ach: Achievement }) {
  const intl = useIntl();
  const def = ACHIEVEMENT_DEFS[ach.id];
  const name = def ? intl.formatMessage({ id: def.nameId }) : ach.id;
  const desc = def ? intl.formatMessage({ id: def.descId }) : undefined;
  const Icon = def?.icon ?? HelpCircle;

  if (!ach.available) {
    return (
      <div className="panel flex items-start gap-3 p-3 opacity-70" aria-label={name}>
        <span className="grid h-11 w-11 shrink-0 place-items-center rounded-card bg-stone-500/10 text-stone-400 ring-1 ring-inset ring-stone-500/15 grayscale dark:text-stone-500">
          <Icon className="h-6 w-6" aria-hidden="true" />
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-semibold text-stone-900 dark:text-stone-50">{name}</p>
          {desc && (
            <p className="mt-0.5 line-clamp-2 text-xs text-stone-500 dark:text-stone-400">{desc}</p>
          )}
          <span
            className="mt-1.5 inline-flex cursor-help items-center rounded-full bg-stone-500/10 px-2 py-0.5 text-[11px] font-medium text-stone-500 dark:text-stone-400"
            title={ach.unavailable_reason ?? undefined}
          >
            {intl.formatMessage({ id: 'growth.ach.unavailable' })}
          </span>
        </div>
      </div>
    );
  }

  const progress =
    ach.progress_denominator > 0 ? ach.progress_current / ach.progress_denominator : 0;

  return (
    <AchievementBadge
      icon={Icon}
      name={name}
      description={desc}
      unlocked={ach.unlocked}
      progress={progress}
      unlockedAt={fmtDate(ach.unlocked_at)}
    />
  );
}
