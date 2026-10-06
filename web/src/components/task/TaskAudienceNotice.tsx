import { useIntl, type IntlShape } from 'react-intl';
import { EyeOff } from 'lucide-react';
import type { TaskAudienceRestriction } from '@/lib/api';

const ACCOUNT_ROLES = new Set(['admin', 'manager', 'employee']);
const TEAM_ROLES = new Set(['planner', 'executor', 'verifier', 'synthesizer']);

/** Localised name of a dashboard account role; unknown values as written. */
function accountRole(intl: IntlShape, role: string): string {
  return ACCOUNT_ROLES.has(role) ? intl.formatMessage({ id: `install.request.role.${role}` }) : role;
}

/** Localised name of the AI role member that wrote a hand-off packet. */
function teamRole(intl: IntlShape, role: string): string {
  return TEAM_ROLES.has(role) ? intl.formatMessage({ id: `settings.team.role.${role}` }) : role;
}

/** Readable summary of audience keys: roles and channels by name, accounts
 *  as a count (their ids are internal). */
function describeKeys(intl: IntlShape, keys: readonly string[]): string {
  const users = keys.filter((k) => k.startsWith('user:')).length;
  const roles = keys.filter((k) => k.startsWith('role:')).map((k) => accountRole(intl, k.slice('role:'.length)));
  const channels = keys.filter((k) => k.startsWith('channel:')).map((k) => k.slice('channel:'.length));
  const parts: string[] = [];
  if (users > 0) parts.push(intl.formatMessage({ id: 'tasks.audience.users' }, { count: users }));
  if (roles.length > 0) parts.push(intl.formatMessage({ id: 'tasks.audience.roles' }, { names: roles.join(', ') }));
  if (channels.length > 0) parts.push(intl.formatMessage({ id: 'tasks.audience.channels' }, { names: channels.join(', ') }));
  return parts.length > 0 ? parts.join('; ') : intl.formatMessage({ id: 'tasks.audience.nobody' });
}

/**
 * Says when a task's content is limited to an audience. The limit comes
 * from team hand-off packets written by AI role members, so the notice names
 * which round and role wrote it. Admins always see the content; a reader
 * outside the audience sees only the card and this notice.
 */
export function TaskAudienceNotice({ restriction, restricted }: { restriction?: TaskAudienceRestriction | null; restricted?: boolean }) {
  const intl = useIntl();
  if (restricted) {
    return (
      <p role="status" className="flex items-start gap-2 rounded-lg border border-border px-3 py-2 text-sm text-muted-foreground">
        <EyeOff className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        {intl.formatMessage({ id: 'tasks.audience.hiddenFromYou' })}
      </p>
    );
  }
  if (!restriction || restriction.state === 'open') return null;
  return (
    <div role="status" className="space-y-1 rounded-lg border border-amber-500/40 px-3 py-2 text-sm text-amber-800 dark:text-amber-300">
      <p className="flex items-start gap-2">
        <EyeOff className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        {restriction.state === 'unreadable'
          ? intl.formatMessage({ id: 'tasks.audience.unreadable' })
          : intl.formatMessage({ id: 'tasks.audience.limited' }, { who: describeKeys(intl, restriction.keys) })}
      </p>
      {restriction.sources.map((s) => (
        <p key={`${s.round}-${s.packet_id}`} className="pl-6 text-xs">
          {intl.formatMessage({ id: 'tasks.audience.source' }, { round: s.round, role: teamRole(intl, s.from_role), who: describeKeys(intl, s.keys) })}
        </p>
      ))}
    </div>
  );
}
