import { Badge } from '@/components/ui';

// Shared read-only "label · value (optional badge)" row used across several
// settings sections (General / Account / Container).
export function SettingRow({
  label,
  value,
  badge,
}: {
  label: string;
  value: string;
  badge?: 'emerald' | 'amber' | 'rose';
}) {
  const badgeTone = { emerald: 'success', amber: 'warning', rose: 'danger' } as const;

  return (
    <div className="flex items-center justify-between border-b border-[var(--panel-border)] pb-3 last:border-0 last:pb-0">
      <span className="text-sm text-stone-600 dark:text-stone-400">
        {label}
      </span>
      {badge ? (
        <Badge tone={badgeTone[badge]}>{value}</Badge>
      ) : (
        <span className="text-sm font-medium text-stone-900 dark:text-stone-50">
          {value}
        </span>
      )}
    </div>
  );
}
