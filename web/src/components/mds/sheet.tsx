import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Dialog as BaseDialog } from '@base-ui/react/dialog';
import { XIcon } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from './button';

/**
 * Sheet — MDS edge-anchored panel (spec §4/§5.2 mobile drawer + right panel),
 * built on @base-ui/react Dialog. `side` picks the anchoring edge.
 */
export const Sheet = BaseDialog.Root;
export const SheetTrigger = BaseDialog.Trigger;
export const SheetClose = BaseDialog.Close;

type SheetSide = 'top' | 'right' | 'bottom' | 'left';

const sideClasses: Record<SheetSide, string> = {
  right:
    'inset-y-0 right-0 h-full w-3/4 max-w-sm border-l data-[starting-style]:translate-x-full data-[ending-style]:translate-x-full',
  left: 'inset-y-0 left-0 h-full w-3/4 max-w-sm border-r data-[starting-style]:-translate-x-full data-[ending-style]:-translate-x-full',
  top: 'inset-x-0 top-0 h-auto border-b data-[starting-style]:-translate-y-full data-[ending-style]:-translate-y-full',
  bottom:
    'inset-x-0 bottom-0 h-auto border-t data-[starting-style]:translate-y-full data-[ending-style]:translate-y-full',
};

export const SheetContent = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseDialog.Popup> & {
    side?: SheetSide;
    showClose?: boolean;
  }
>(({ className, children, side = 'right', showClose = true, ...props }, ref) => (
  <BaseDialog.Portal>
    <BaseDialog.Backdrop
      data-slot="sheet-overlay"
      className={cn(
        'fixed inset-0 z-50 bg-black/10 backdrop-blur-xs transition-opacity duration-200',
        'data-[starting-style]:opacity-0 data-[ending-style]:opacity-0'
      )}
    />
    <BaseDialog.Popup
      ref={ref}
      data-slot="sheet-content"
      data-side={side}
      className={cn(
        'fixed z-50 flex flex-col gap-4 bg-surface-raised p-4 text-sm text-surface-foreground shadow-[var(--floating-shadow)] ring-1 ring-surface-border',
        'transition-transform duration-200 ease-out',
        sideClasses[side],
        className
      )}
      {...props}
    >
      {children}
      {showClose && (
        <BaseDialog.Close
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Close"
              className="absolute top-3 right-3"
            />
          }
        >
          <XIcon />
        </BaseDialog.Close>
      )}
    </BaseDialog.Popup>
  </BaseDialog.Portal>
));
SheetContent.displayName = 'SheetContent';

export function SheetHeader({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="sheet-header"
      className={cn('grid gap-1.5', className)}
      {...props}
    />
  );
}

export const SheetTitle = forwardRef<
  HTMLHeadingElement,
  ComponentPropsWithoutRef<typeof BaseDialog.Title>
>(({ className, ...props }, ref) => (
  <BaseDialog.Title
    ref={ref}
    data-slot="sheet-title"
    className={cn('text-base font-medium leading-none', className)}
    {...props}
  />
));
SheetTitle.displayName = 'SheetTitle';

export const SheetDescription = forwardRef<
  HTMLParagraphElement,
  ComponentPropsWithoutRef<typeof BaseDialog.Description>
>(({ className, ...props }, ref) => (
  <BaseDialog.Description
    ref={ref}
    data-slot="sheet-description"
    className={cn('text-sm text-muted-foreground', className)}
    {...props}
  />
));
SheetDescription.displayName = 'SheetDescription';

export function SheetFooter({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="sheet-footer"
      className={cn('mt-auto flex flex-col gap-2', className)}
      {...props}
    />
  );
}
