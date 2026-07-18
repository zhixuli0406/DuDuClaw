import { type ReactNode } from 'react';
import { AlertTriangle } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * DangerZone — a rose-bordered container for destructive or high-impact
 * settings (network exposure, extra mounts, permission escalation, data wipe).
 * Wrap the risky controls; the visual boundary tells the user "handle with
 * care". Destructive *actions* still route through <ConfirmDialog>.
 */
export function DangerZone({
  title,
  description,
  children,
  className,
}: {
  title?: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        'rounded-xl border border-destructive/30 bg-destructive/5 p-4',
        className,
      )}
    >
      {(title || description) && (
        <div className="mb-3 flex items-start gap-2">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
          <div className="min-w-0">
            {title && (
              <h4 className="text-sm font-semibold text-destructive">{title}</h4>
            )}
            {description && (
              <p className="mt-0.5 text-xs text-destructive/80">{description}</p>
            )}
          </div>
        </div>
      )}
      <div className="space-y-4">{children}</div>
    </div>
  );
}
