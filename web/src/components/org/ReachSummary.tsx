import { useIntl } from 'react-intl';
import type { ReachLine } from '@/lib/api';
import { useSystemStore } from '@/stores/system-store';

/** Render only public display fields; server sentences are never used. */
export function ReachSummary({ reach, applied = false }: { reach: ReachLine[] | null | undefined; applied?: boolean }) {
  const intl = useIntl();
  const personal = useSystemStore(s => s.status?.edition_profile === 'personal');
  if (personal || reach === undefined) return null;
  const t = (key: string, values?: Record<string, string>) => intl.formatMessage({ id: `orgReach.${key}` }, values);
  const text = reach === null ? t('unavailable') : reach.length === 0 ? t('empty') :
    t(applied ? 'applied' : 'approved') + reach.map(line => {
      const groups = line.groups.map(group => t(`group.${group}`));
      const joined = groups.length < 2 ? groups.join('') :
        groups.slice(0, -1).join(t('listSeparator')) + t('and') + groups[groups.length - 1];
      return t(line.change, { name: line.display_name, department: line.department, groups: joined });
    }).join(t('clauseSeparator')) + t('period');
  return <div className="col-span-full space-y-1 rounded-lg bg-muted p-3 text-sm" data-testid="org-reach">
    <p className="text-xs font-medium text-muted-foreground">{t('label')}</p>
    <p className="break-words">{text}</p>
  </div>;
}
