import { Link } from 'react-router';
import { useIntl } from 'react-intl';
import { ChevronRight, Settings } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { filterVisible } from '@/lib/nav-visibility';
import { manageAdvancedNav } from '@/components/layout/nav-model';
import { Empty } from '@/components/mds';

/**
 * 進階設定 — the index for every management surface that did not earn one of the
 * four promoted rail slots (2026-08-04, D18). Nothing here is new and nothing
 * was removed; this page exists so the folded surfaces have a home you can land
 * on, and so `/manage/advanced` is a real destination rather than a dead label.
 *
 * Each card links to the page's own unchanged route, so bookmarks, ⌘K and deep
 * links keep working exactly as before.
 */
export function ManageAdvancedPage() {
  const intl = useIntl();
  const role = useAuthStore((s) => s.user?.role);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const items = filterVisible(manageAdvancedNav, role, isPersonal);

  if (items.length === 0) {
    return (
      <Empty
        icon={Settings}
        title={intl.formatMessage({ id: 'manage.advanced.empty' })}
        variant="dashed"
      />
    );
  }

  return (
    <div className="mx-auto max-w-3xl space-y-5">
      <div>
        <h2 className="text-xl font-semibold text-foreground sm:text-2xl">
          {intl.formatMessage({ id: 'manage.advanced' })}
        </h2>
        <p className="mt-1 text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'manage.advanced.intro' })}
        </p>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        {items.map((item) => (
          <Link
            key={item.to}
            to={item.to}
            className="group flex items-start gap-3 rounded-xl border border-surface-border bg-card p-4 outline-none transition-colors hover:bg-surface-hover focus-visible:ring-3 focus-visible:ring-ring/50"
          >
            <span className="mt-0.5 grid size-8 shrink-0 place-items-center rounded-lg bg-muted text-muted-foreground">
              <item.icon className="size-4" />
            </span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-medium text-foreground">
                {intl.formatMessage({ id: item.label })}
              </span>
              <span className="mt-0.5 block text-xs text-muted-foreground">
                {intl.formatMessage({ id: item.desc })}
              </span>
            </span>
            <ChevronRight className="mt-1 size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
          </Link>
        ))}
      </div>
    </div>
  );
}
