import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Separator as BaseSeparator } from '@base-ui/react/separator';
import { cn } from '@/lib/utils';

/** Separator — MDS divider (spec §5.1), built on @base-ui/react. */
export const Separator = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseSeparator> & {
    orientation?: 'horizontal' | 'vertical';
  }
>(({ className, orientation = 'horizontal', ...props }, ref) => (
  <BaseSeparator
    ref={ref}
    data-slot="separator"
    orientation={orientation}
    className={cn(
      'shrink-0 bg-border',
      orientation === 'horizontal' ? 'h-px w-full' : 'h-full w-px',
      className
    )}
    {...props}
  />
));
Separator.displayName = 'Separator';
