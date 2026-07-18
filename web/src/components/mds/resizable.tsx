import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Group, Panel, Separator } from 'react-resizable-panels';
import { cn } from '@/lib/utils';

/**
 * Resizable — MDS split-pane wrappers over react-resizable-panels v4
 * (spec §5.2 list+detail split). `ResizablePanelGroup` = Group,
 * `ResizablePanel` = Panel, `ResizableHandle` = Separator. Sizes are px
 * (number) or percentage (unit-less string), per the underlying library.
 */

export const ResizablePanelGroup = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof Group>
>(({ className, ...props }, ref) => (
  <Group
    elementRef={ref}
    data-slot="resizable-panel-group"
    className={cn(
      'flex h-full w-full data-[orientation=vertical]:flex-col',
      className
    )}
    {...props}
  />
));
ResizablePanelGroup.displayName = 'ResizablePanelGroup';

export const ResizablePanel = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof Panel>
>(({ className, ...props }, ref) => (
  <Panel
    elementRef={ref}
    data-slot="resizable-panel"
    className={cn('min-h-0 min-w-0', className)}
    {...props}
  />
));
ResizablePanel.displayName = 'ResizablePanel';

export const ResizableHandle = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof Separator>
>(({ className, ...props }, ref) => (
  <Separator
    elementRef={ref}
    data-slot="resizable-handle"
    className={cn(
      'group relative flex items-center justify-center bg-surface-border outline-none',
      // 1px seam that thickens to 2px on hover/drag, in both orientations.
      'data-[orientation=horizontal]:w-px data-[orientation=horizontal]:cursor-ew-resize',
      'data-[orientation=vertical]:h-px data-[orientation=vertical]:cursor-ns-resize',
      'transition-colors hover:bg-sidebar-border data-[dragging]:bg-sidebar-border',
      'hover:data-[orientation=horizontal]:w-0.5 data-[dragging]:data-[orientation=horizontal]:w-0.5',
      'hover:data-[orientation=vertical]:h-0.5 data-[dragging]:data-[orientation=vertical]:h-0.5',
      'focus-visible:ring-2 focus-visible:ring-ring/50',
      className
    )}
    {...props}
  />
));
ResizableHandle.displayName = 'ResizableHandle';
