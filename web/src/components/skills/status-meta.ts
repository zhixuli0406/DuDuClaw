/**
 * Presentation metadata for the six custom-skill statuses (§5.6). Shared by the
 * "我的技能" section chips, the detail page, and anywhere a status badge shows.
 * Pure data so the mapping stays in one place (fail-safe default = neutral).
 */
import type { IntlShape } from 'react-intl';
import type { CustomSkillStatus } from '@/lib/api-custom-skills';

/** Badge `tone` values available in the UI library. */
export type StatusTone = 'neutral' | 'success' | 'warning' | 'danger' | 'info';

export interface StatusMeta {
  /** i18n key for the human label. */
  labelKey: string;
  tone: StatusTone;
}

const META: Record<CustomSkillStatus, StatusMeta> = {
  draft: { labelKey: 'skills.custom.status.draft', tone: 'neutral' },
  generating: { labelKey: 'skills.custom.status.generating', tone: 'info' },
  pending_approval: { labelKey: 'skills.custom.status.pending', tone: 'warning' },
  approved: { labelKey: 'skills.custom.status.approved', tone: 'success' },
  rejected: { labelKey: 'skills.custom.status.rejected', tone: 'danger' },
  retired: { labelKey: 'skills.custom.status.retired', tone: 'neutral' },
};

export function statusMeta(status: CustomSkillStatus): StatusMeta {
  return META[status] ?? { labelKey: 'skills.custom.status.draft', tone: 'neutral' };
}

/**
 * Map a semantic `StatusTone` onto MDS `Badge` styling (spec §4 Badge only ships
 * default/secondary/destructive/outline/ghost variants, so the success/warning/
 * info tones are expressed with semantic-token className overrides). Returns the
 * base variant plus the extra classes so callers write `<Badge variant className>`.
 */
export function statusToneBadge(tone: StatusTone): {
  variant: 'secondary' | 'destructive';
  className?: string;
} {
  switch (tone) {
    case 'success':
      return { variant: 'secondary', className: 'bg-success/15 text-success' };
    case 'warning':
      return { variant: 'secondary', className: 'bg-warning/15 text-warning' };
    case 'info':
      return { variant: 'secondary', className: 'bg-info/15 text-info' };
    case 'danger':
      return { variant: 'destructive' };
    case 'neutral':
    default:
      return { variant: 'secondary' };
  }
}

/** Semantic dot colour for a `StatusTone` (small coloured status dot in rows). */
export function statusToneDot(tone: StatusTone): string {
  switch (tone) {
    case 'success':
      return 'bg-success';
    case 'warning':
      return 'bg-warning';
    case 'info':
      return 'bg-info';
    case 'danger':
      return 'bg-destructive';
    case 'neutral':
    default:
      return 'bg-muted-foreground';
  }
}

/**
 * Format a self-reported time-saving estimate. The unit selects a per-unit
 * message key ("分鐘/次" vs "小時/月") so the wording lives in the catalogue;
 * the `{value}` placeholder is substituted here (passing values as the second
 * `formatMessage` arg — a bundled descriptor would NOT interpolate).
 */
export function formatTimeSaved(intl: IntlShape, value: number, unit: string): string {
  const id =
    unit === 'hours_per_month' ? 'skills.custom.timeSaved.hoursPerMonth' : 'skills.custom.timeSaved.minutesPerUse';
  return intl.formatMessage({ id }, { value });
}
