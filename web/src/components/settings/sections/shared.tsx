import { Badge, SettingsRow } from '@/components/mds';

// Shared read-only "label · value (optional badge)" row used across several
// settings sections (General / Account / Container). An mds SettingsRow with the
// value rendered as the right-hand control content.
export function SettingRow({
  label,
  value,
  badge,
}: {
  label: string;
  value: string;
  badge?: 'emerald' | 'amber' | 'rose';
}) {
  const badgeClass = {
    emerald: 'bg-success/10 text-success',
    amber: 'bg-warning/10 text-warning',
    rose: 'bg-destructive/10 text-destructive',
  } as const;

  return (
    <SettingsRow label={label}>
      {badge ? (
        <Badge className={badgeClass[badge]}>{value}</Badge>
      ) : (
        <span className="text-sm font-medium text-foreground">{value}</span>
      )}
    </SettingsRow>
  );
}
