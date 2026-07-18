import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Tooltip as BaseTooltip } from '@base-ui/react/tooltip';
import { cn } from '@/lib/utils';

/**
 * Tooltip — MDS hover hint (spec §4 Tooltip), built on @base-ui/react.
 * Lighter than menus/popovers: border + bg-popover, 500ms open delay.
 */
export const Tooltip = BaseTooltip.Root;
export const TooltipTrigger = BaseTooltip.Trigger;

/** Shared 500ms open delay for grouped tooltips (spec §3 / §4 Tooltip). */
export function TooltipProvider({
  delay = 500,
  ...props
}: ComponentPropsWithoutRef<typeof BaseTooltip.Provider>) {
  return <BaseTooltip.Provider delay={delay} {...props} />;
}

export const TooltipContent = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseTooltip.Popup> & { sideOffset?: number }
>(({ className, sideOffset = 4, ...props }, ref) => (
  <BaseTooltip.Portal>
    <BaseTooltip.Positioner sideOffset={sideOffset} className="z-50">
      <BaseTooltip.Popup
        ref={ref}
        data-slot="tooltip-content"
        className={cn(
          'w-fit max-w-xs rounded-lg border border-border bg-popover px-2.5 py-1 text-xs text-popover-foreground',
          'origin-[var(--transform-origin)] transition-[transform,opacity] duration-100',
          'data-[starting-style]:scale-95 data-[starting-style]:opacity-0',
          'data-[ending-style]:scale-95 data-[ending-style]:opacity-0',
          className
        )}
        {...props}
      />
    </BaseTooltip.Positioner>
  </BaseTooltip.Portal>
));
TooltipContent.displayName = 'TooltipContent';
