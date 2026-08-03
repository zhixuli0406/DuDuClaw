import { useIntl } from 'react-intl';
import { UpdateTab } from '@/components/settings/sections/UpdateTab';

/**
 * 系統更新 — lifted out of the settings page's tab rail into its own management
 * entry (2026-08-04, D18). It was reported as "buried"; a version check people
 * run after every release should not be the fifth tab of a settings page.
 *
 * The panel itself is unchanged — this is a placement change, not a rewrite.
 */
export function SystemUpdatePage() {
  const intl = useIntl();
  return (
    <div className="mx-auto max-w-3xl space-y-5">
      <div>
        <h2 className="text-xl font-semibold text-foreground sm:text-2xl">
          {intl.formatMessage({ id: 'manage.updates' })}
        </h2>
        <p className="mt-1 text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'manage.updates.desc' })}
        </p>
      </div>
      <UpdateTab />
    </div>
  );
}
