import { useIntl } from 'react-intl';
import { User, Building2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useSystemStore } from '@/stores/system-store';

/**
 * Small chip in the sidebar footer showing the active product form-factor
 * (Personal / Enterprise). Reads `system.status.edition_profile`. An absent
 * value (older gateway) renders nothing, so existing installs are unaffected.
 *
 * This is presentation only — it never gates a feature.
 */
export function EditionBadge() {
  const intl = useIntl();
  const profile = useSystemStore((s) => s.status?.edition_profile);
  if (!profile) return null;

  const isPersonal = profile === 'personal';
  const Icon = isPersonal ? User : Building2;
  const labelId = isPersonal ? 'edition.personal' : 'edition.enterprise';

  return (
    <span
      className={cn(
        'inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[10px] font-medium',
        isPersonal
          ? 'bg-brand/12 text-brand'
          : 'bg-muted text-muted-foreground'
      )}
      title={intl.formatMessage({ id: labelId })}
    >
      <Icon className="h-3 w-3" />
      {intl.formatMessage({ id: labelId })}
    </span>
  );
}
