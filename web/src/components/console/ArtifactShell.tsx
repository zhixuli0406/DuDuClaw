import type { ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Link } from 'react-router';
import { ArrowRight, type LucideIcon } from 'lucide-react';
import { Card, CardHeader, CardTitle, CardContent } from '@/components/mds';
import { cn } from '@/lib/utils';

/**
 * Shared shell every artifact card renders through — one consistent visual
 * identity for "a structured result embedded in the conversation" (§6.2
 * "結果內嵌、不跳 app"), instead of each card reinventing spacing/animation.
 *
 * `advancedTo` renders the §6.5 O-5 affordance: a small "在OO頁查看完整內容"
 * link to the native settings page this card summarizes. It is deliberately
 * NOT the primary way to see the data — the card already shows it inline —
 * it's the "one step away, never removed" escape hatch the design doc
 * requires every capability to keep.
 */
export function ArtifactShell({
  icon: Icon,
  title,
  advancedTo,
  advancedLabel,
  children,
  className,
}: {
  icon: LucideIcon;
  title: ReactNode;
  advancedTo?: string;
  advancedLabel?: string;
  children: ReactNode;
  className?: string;
}) {
  const intl = useIntl();
  return (
    <div className="flex justify-start">
      {/* animate-fade-up is neutralized under prefers-reduced-motion via the
          global `* { animation-duration: 0.01ms }` rule in index.css — no
          separate reduced-motion branch needed here. */}
      <div className={cn('w-full max-w-md animate-fade-up', className)}>
        <Card data-size="sm">
          <CardHeader className="flex flex-row items-center gap-2 space-y-0">
            <Icon className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <CardTitle className="text-sm font-medium">{title}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {children}
            {advancedTo && (
              <Link
                to={advancedTo}
                className="inline-flex items-center gap-1 text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
              >
                {advancedLabel ?? intl.formatMessage({ id: 'console.artifact.viewFull' })}
                <ArrowRight className="size-3" aria-hidden="true" />
              </Link>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
