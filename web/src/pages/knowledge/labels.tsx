import { useIntl } from 'react-intl';
import { Globe, Lock, ShieldCheck, Users } from 'lucide-react';
import { Badge } from '@/components/mds';
import { cn } from '@/lib/utils';
import type { KbSensitivity } from '@/lib/knowledge-api';

const SENSITIVITY_STYLE: Record<KbSensitivity, { icon: typeof Globe; className: string }> = {
  public: { icon: Globe, className: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-400' },
  internal: { icon: Users, className: 'bg-muted text-muted-foreground' },
  personal: { icon: ShieldCheck, className: 'bg-amber-500/15 text-amber-700 dark:text-amber-400' },
  restricted: { icon: Lock, className: 'bg-destructive/10 text-destructive' },
};

/** 敏感度 badge (公開 / 公司內部 / 個人 / 機密). */
export function SensitivityBadge({ value, className }: { value: KbSensitivity; className?: string }) {
  const intl = useIntl();
  const style = SENSITIVITY_STYLE[value] ?? SENSITIVITY_STYLE.internal;
  const Icon = style.icon;
  return (
    <Badge variant="ghost" className={cn(style.className, className)}>
      <Icon aria-hidden />
      {intl.formatMessage({ id: `kb.sensitivity.${value}` })}
    </Badge>
  );
}

/** Local date-time for a stored RFC 3339 timestamp; the raw value when unparsable. */
export function formatWhen(intl: ReturnType<typeof useIntl>, iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return intl.formatDate(d, { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
}

/** Human file size. */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
