import { useMemo } from 'react';
import { useIntl } from 'react-intl';
import type { UserDetail } from '@/lib/api';
import { managerCandidates } from '@/lib/org-tree';
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from '@/components/mds';

/** Select value for "no direct manager" (Select needs a non-empty value). */
const NONE = '__none__';

/**
 * 直屬主管 picker for the create / edit user dialogs. Offers active managers
 * and admins, minus the person themselves and everyone below them; the
 * gateway re-checks every rule on save.
 *
 * `value` is a user id or '' for none. A current manager that is no longer
 * a valid choice (suspended, demoted) still shows by name, so opening the
 * dialog never silently changes it.
 */
export function ManagerSelect({
  users,
  userId,
  value,
  onChange,
}: {
  users: UserDetail[];
  /** The person being edited; null while creating one. */
  userId: string | null;
  value: string;
  onChange: (managerId: string) => void;
}) {
  const intl = useIntl();
  const candidates = useMemo(() => managerCandidates(users, userId), [users, userId]);
  const noneLabel = intl.formatMessage({ id: 'users.manager.none' });
  const current = users.find((u) => u.id === value);
  const options = [
    { value: NONE, label: noneLabel },
    ...candidates.map((u) => ({ value: u.id, label: u.display_name })),
    ...(current && !candidates.some((c) => c.id === current.id)
      ? [{ value: current.id, label: current.display_name }]
      : []),
  ];
  const selected = value || NONE;

  return (
    <Select value={selected} onValueChange={(v) => onChange(v === NONE ? '' : String(v))}>
      <SelectTrigger className="w-full" aria-label={intl.formatMessage({ id: 'users.manager.label' })}>
        <SelectValue>{options.find((o) => o.value === selected)?.label ?? noneLabel}</SelectValue>
      </SelectTrigger>
      <SelectContent>
        {options.map((o) => (
          <SelectItem key={o.value} value={o.value}>
            {o.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
