import { forwardRef, type HTMLAttributes } from 'react';
import { cn } from '@/lib/utils';

/**
 * Card — MDS primitive (spec §4 Card). `data-size="sm"` tightens gap/padding.
 * Compose with Card.Header / Title / Description / Content / Footer.
 */
export const Card = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement>>(
  ({ className, ...props }, ref) => (
    <div
      ref={ref}
      data-slot="card"
      className={cn(
        'flex flex-col gap-4 overflow-hidden rounded-xl border border-surface-border bg-surface py-4 text-sm text-surface-foreground shadow-[var(--surface-shadow)]',
        'data-[size=sm]:gap-3 data-[size=sm]:py-3',
        className
      )}
      {...props}
    />
  )
);
Card.displayName = 'Card';

export const CardHeader = forwardRef<
  HTMLDivElement,
  HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="card-header"
    className={cn(
      'grid gap-1 px-4 has-data-[slot=card-action]:grid-cols-[1fr_auto]',
      className
    )}
    {...props}
  />
));
CardHeader.displayName = 'CardHeader';

export const CardTitle = forwardRef<
  HTMLDivElement,
  HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="card-title"
    className={cn('text-base font-medium leading-snug', className)}
    {...props}
  />
));
CardTitle.displayName = 'CardTitle';

export const CardDescription = forwardRef<
  HTMLDivElement,
  HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="card-description"
    className={cn('text-sm text-muted-foreground', className)}
    {...props}
  />
));
CardDescription.displayName = 'CardDescription';

export const CardAction = forwardRef<
  HTMLDivElement,
  HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="card-action"
    className={cn('col-start-2 row-span-2 row-start-1 self-start', className)}
    {...props}
  />
));
CardAction.displayName = 'CardAction';

export const CardContent = forwardRef<
  HTMLDivElement,
  HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="card-content"
    className={cn('px-4', className)}
    {...props}
  />
));
CardContent.displayName = 'CardContent';

export const CardFooter = forwardRef<
  HTMLDivElement,
  HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="card-footer"
    className={cn(
      'flex items-center border-t border-surface-border bg-surface-hover/70 p-4',
      className
    )}
    {...props}
  />
));
CardFooter.displayName = 'CardFooter';
