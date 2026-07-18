import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Popover as BasePopover } from '@base-ui/react/popover';
import { cn } from '@/lib/utils';

/**
 * Popover — MDS floating panel (spec §4 / §5.4 Display popover), built on
 * @base-ui/react. Compose: Popover > PopoverTrigger + PopoverContent.
 */
export const Popover = BasePopover.Root;
export const PopoverTrigger = BasePopover.Trigger;
export const PopoverClose = BasePopover.Close;

export const PopoverContent = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BasePopover.Popup> & {
    sideOffset?: number;
    align?: 'start' | 'center' | 'end';
  }
>(({ className, sideOffset = 4, align = 'center', ...props }, ref) => (
  <BasePopover.Portal>
    <BasePopover.Positioner
      sideOffset={sideOffset}
      align={align}
      className="z-50"
    >
      <BasePopover.Popup
        ref={ref}
        data-slot="popover-content"
        className={cn(
          'w-72 rounded-lg bg-surface-raised p-4 text-sm text-surface-foreground shadow-[var(--menu-shadow)] ring-1 ring-surface-border outline-none',
          'origin-[var(--transform-origin)] transition-[transform,opacity] duration-100',
          'data-[starting-style]:scale-95 data-[starting-style]:opacity-0',
          'data-[ending-style]:scale-95 data-[ending-style]:opacity-0',
          className
        )}
        {...props}
      />
    </BasePopover.Positioner>
  </BasePopover.Portal>
));
PopoverContent.displayName = 'PopoverContent';
