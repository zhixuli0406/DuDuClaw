import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { filterVisible } from '@/lib/nav-visibility';
import { LauncherCard } from './LauncherCard';
import {
  LAUNCHER_CARDS,
  LAUNCHER_GROUP_ORDER,
  type LauncherCardModel,
  type LauncherGroupKey,
} from './launcher-model';

/**
 * The grouped capability launcher (TODO-genspark-workspace-shell §P3.2),
 * Genspark's tool grid mapped to DuDuClaw. Role/edition filtering reuses the
 * exact predicate the sidebar uses (`filterVisible`) so visibility never drifts.
 */
export function LauncherGrid() {
  const intl = useIntl();
  const role = useAuthStore((s) => s.user?.role);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile === 'personal');

  const visible = filterVisible(LAUNCHER_CARDS, role, isPersonal);
  const byGroup = new Map<LauncherGroupKey, LauncherCardModel[]>();
  for (const card of visible) {
    const list = byGroup.get(card.group) ?? [];
    list.push(card);
    byGroup.set(card.group, list);
  }

  return (
    <div className="space-y-6">
      {LAUNCHER_GROUP_ORDER.map((group) => {
        const cards = byGroup.get(group);
        if (!cards || cards.length === 0) return null;
        return (
          <section key={group} aria-labelledby={`launcher-group-${group}`}>
            <h3
              id={`launcher-group-${group}`}
              className="mb-2 px-1 text-[11px] font-semibold uppercase tracking-wider text-stone-400"
            >
              {intl.formatMessage({ id: `launcher.group.${group}` })}
            </h3>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4">
              {cards.map((card) => (
                <LauncherCard key={card.id} card={card} />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}
