import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * Page — max-width container that establishes the vertical rhythm for every
 * route. Wrap each page's content in this so spacing is consistent (Calm Glass
 * §2). Use `wide` for data-dense pages (boards, tables) that need more room.
 */
export function Page({
  children,
  className,
  wide = false,
}: {
  children: ReactNode;
  className?: string;
  wide?: boolean;
}) {
  return (
    <div
      className={cn(
        'mx-auto w-full space-y-6',
        wide ? 'max-w-[1440px]' : 'max-w-[1200px]',
        className
      )}
    >
      {children}
    </div>
  );
}
