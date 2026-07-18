import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Menu as BaseMenu } from '@base-ui/react/menu';
import { cn } from '@/lib/utils';

/**
 * DropdownMenu — MDS action menu (spec §4 DropdownMenu), built on
 * @base-ui/react Menu. Compose: DropdownMenu > DropdownMenuTrigger +
 * DropdownMenuContent > DropdownMenuItem / Label / Separator / Shortcut.
 */
export const DropdownMenu = BaseMenu.Root;
export const DropdownMenuTrigger = BaseMenu.Trigger;
export const DropdownMenuGroup = BaseMenu.Group;

export const DropdownMenuContent = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseMenu.Popup> & { sideOffset?: number }
>(({ className, sideOffset = 4, ...props }, ref) => (
  <BaseMenu.Portal>
    <BaseMenu.Positioner sideOffset={sideOffset} className="z-50">
      <BaseMenu.Popup
        ref={ref}
        data-slot="dropdown-menu-content"
        className={cn(
          'min-w-32 rounded-lg bg-surface-raised p-1 text-surface-foreground shadow-[var(--menu-shadow)] ring-1 ring-surface-border outline-none',
          'origin-[var(--transform-origin)] transition-[transform,opacity] duration-100',
          'data-[starting-style]:scale-95 data-[starting-style]:opacity-0',
          'data-[ending-style]:scale-95 data-[ending-style]:opacity-0',
          className
        )}
        {...props}
      />
    </BaseMenu.Positioner>
  </BaseMenu.Portal>
));
DropdownMenuContent.displayName = 'DropdownMenuContent';

export const DropdownMenuItem = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseMenu.Item> & {
    variant?: 'default' | 'destructive';
  }
>(({ className, variant = 'default', ...props }, ref) => (
  <BaseMenu.Item
    ref={ref}
    data-slot="dropdown-menu-item"
    data-variant={variant}
    className={cn(
      'relative flex cursor-default select-none items-center gap-2 rounded-md px-1.5 py-1 text-sm outline-none',
      'data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground',
      'data-[disabled]:pointer-events-none data-[disabled]:opacity-50',
      'data-[variant=destructive]:text-destructive data-[variant=destructive]:data-[highlighted]:bg-destructive/10 data-[variant=destructive]:data-[highlighted]:text-destructive',
      "[&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4",
      className
    )}
    {...props}
  />
));
DropdownMenuItem.displayName = 'DropdownMenuItem';

/**
 * Plain label (spec §4). For a semantically-grouped label use
 * `<DropdownMenuGroup>` + `<BaseMenu.GroupLabel>`; this standalone variant is a
 * div so it can head an ungrouped section without a Group wrapper.
 */
export const DropdownMenuLabel = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="dropdown-menu-label"
    className={cn('px-1.5 py-1 text-xs font-medium text-muted-foreground', className)}
    {...props}
  />
));
DropdownMenuLabel.displayName = 'DropdownMenuLabel';

export const DropdownMenuSeparator = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="dropdown-menu-separator"
    className={cn('-mx-1 my-1 h-px bg-border', className)}
    {...props}
  />
));
DropdownMenuSeparator.displayName = 'DropdownMenuSeparator';

export function DropdownMenuShortcut({
  className,
  ...props
}: ComponentPropsWithoutRef<'span'>) {
  return (
    <span
      data-slot="dropdown-menu-shortcut"
      className={cn(
        'ml-auto text-xs tracking-widest text-muted-foreground',
        className
      )}
      {...props}
    />
  );
}
