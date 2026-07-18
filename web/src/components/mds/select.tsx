import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Select as BaseSelect } from '@base-ui/react/select';
import { CheckIcon, ChevronDownIcon } from 'lucide-react';
import { cn } from '@/lib/utils';

/**
 * Select — MDS dropdown select (spec §4 Select), built on @base-ui/react.
 * Compose: Select > SelectTrigger(SelectValue) + SelectContent > SelectItem.
 */
export const Select = BaseSelect.Root;
export const SelectGroup = BaseSelect.Group;
export const SelectValue = BaseSelect.Value;

export const SelectTrigger = forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<typeof BaseSelect.Trigger> & {
    size?: 'sm' | 'default';
  }
>(({ className, children, size = 'default', ...props }, ref) => (
  <BaseSelect.Trigger
    ref={ref}
    data-slot="select-trigger"
    data-size={size}
    className={cn(
      'flex w-fit items-center justify-between gap-2 rounded-lg border border-input bg-transparent py-2 pr-2 pl-2.5 text-sm outline-none',
      'data-[size=sm]:h-7 data-[size=default]:h-8',
      'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
      'data-[disabled]:pointer-events-none data-[disabled]:opacity-50',
      'aria-invalid:border-destructive dark:bg-input/30',
      "[&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4",
      className
    )}
    {...props}
  >
    {children}
    <BaseSelect.Icon className="text-muted-foreground">
      <ChevronDownIcon className="size-4" />
    </BaseSelect.Icon>
  </BaseSelect.Trigger>
));
SelectTrigger.displayName = 'SelectTrigger';

export const SelectContent = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseSelect.Popup> & { sideOffset?: number }
>(({ className, children, sideOffset = 4, ...props }, ref) => (
  <BaseSelect.Portal>
    <BaseSelect.Positioner sideOffset={sideOffset} className="z-50">
      <BaseSelect.Popup
        ref={ref}
        data-slot="select-content"
        className={cn(
          'max-h-[min(24rem,var(--available-height))] min-w-36 overflow-y-auto overflow-x-hidden rounded-lg bg-surface-raised p-1 text-surface-foreground shadow-[var(--menu-shadow)] ring-1 ring-surface-border',
          'origin-[var(--transform-origin)] transition-[transform,opacity] duration-100',
          'data-[starting-style]:scale-95 data-[starting-style]:opacity-0',
          'data-[ending-style]:scale-95 data-[ending-style]:opacity-0',
          className
        )}
        {...props}
      >
        {children}
      </BaseSelect.Popup>
    </BaseSelect.Positioner>
  </BaseSelect.Portal>
));
SelectContent.displayName = 'SelectContent';

export const SelectItem = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseSelect.Item>
>(({ className, children, ...props }, ref) => (
  <BaseSelect.Item
    ref={ref}
    data-slot="select-item"
    className={cn(
      'relative flex w-full cursor-default select-none items-center gap-2 rounded-md py-1 pr-8 pl-1.5 text-sm outline-none',
      'data-[highlighted]:bg-accent data-[highlighted]:text-accent-foreground',
      'data-[disabled]:pointer-events-none data-[disabled]:opacity-50',
      "[&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4",
      className
    )}
    {...props}
  >
    <BaseSelect.ItemText>{children}</BaseSelect.ItemText>
    <BaseSelect.ItemIndicator className="absolute right-2 flex items-center">
      <CheckIcon className="size-4" />
    </BaseSelect.ItemIndicator>
  </BaseSelect.Item>
));
SelectItem.displayName = 'SelectItem';

export const SelectLabel = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseSelect.GroupLabel>
>(({ className, ...props }, ref) => (
  <BaseSelect.GroupLabel
    ref={ref}
    data-slot="select-label"
    className={cn('px-1.5 py-1 text-xs text-muted-foreground', className)}
    {...props}
  />
));
SelectLabel.displayName = 'SelectLabel';

export const SelectSeparator = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<'div'>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    data-slot="select-separator"
    className={cn('-mx-1 my-1 h-px bg-border', className)}
    {...props}
  />
));
SelectSeparator.displayName = 'SelectSeparator';
