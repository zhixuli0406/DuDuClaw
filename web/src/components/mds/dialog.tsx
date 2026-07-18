import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Dialog as BaseDialog } from '@base-ui/react/dialog';
import { XIcon } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from './button';

/**
 * Dialog — MDS centered modal (spec §4 Dialog), built on @base-ui/react.
 * Compose: Dialog > DialogTrigger + DialogContent > DialogHeader/Title/Footer.
 */
export const Dialog = BaseDialog.Root;
export const DialogTrigger = BaseDialog.Trigger;
export const DialogClose = BaseDialog.Close;

export const DialogContent = forwardRef<
  HTMLDivElement,
  ComponentPropsWithoutRef<typeof BaseDialog.Popup> & {
    showClose?: boolean;
  }
>(({ className, children, showClose = true, ...props }, ref) => (
  <BaseDialog.Portal>
    <BaseDialog.Backdrop
      data-slot="dialog-overlay"
      className={cn(
        'fixed inset-0 z-50 bg-black/10 backdrop-blur-xs transition-opacity duration-100',
        'data-[starting-style]:opacity-0 data-[ending-style]:opacity-0'
      )}
    />
    <BaseDialog.Popup
      ref={ref}
      data-slot="dialog-content"
      className={cn(
        'fixed top-1/2 left-1/2 z-50 grid w-full max-w-[calc(100%-2rem)] -translate-x-1/2 -translate-y-1/2 gap-4 rounded-xl bg-surface-raised p-4 text-sm text-surface-foreground shadow-[var(--floating-shadow)] ring-1 ring-surface-border sm:max-w-sm',
        'transition-[transform,opacity] duration-100',
        'data-[starting-style]:scale-95 data-[starting-style]:opacity-0',
        'data-[ending-style]:scale-95 data-[ending-style]:opacity-0',
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
DialogContent.displayName = 'DialogContent';

export function DialogHeader({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="dialog-header"
      className={cn('grid gap-1.5', className)}
      {...props}
    />
  );
}

export const DialogTitle = forwardRef<
  HTMLHeadingElement,
  ComponentPropsWithoutRef<typeof BaseDialog.Title>
>(({ className, ...props }, ref) => (
  <BaseDialog.Title
    ref={ref}
    data-slot="dialog-title"
    className={cn('text-base font-medium leading-none', className)}
    {...props}
  />
));
DialogTitle.displayName = 'DialogTitle';

export const DialogDescription = forwardRef<
  HTMLParagraphElement,
  ComponentPropsWithoutRef<typeof BaseDialog.Description>
>(({ className, ...props }, ref) => (
  <BaseDialog.Description
    ref={ref}
    data-slot="dialog-description"
    className={cn('text-sm text-muted-foreground', className)}
    {...props}
  />
));
DialogDescription.displayName = 'DialogDescription';

export function DialogFooter({
  className,
  ...props
}: ComponentPropsWithoutRef<'div'>) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn(
        '-mx-4 -mb-4 flex flex-col-reverse gap-2 border-t border-surface-border bg-surface-hover/70 p-4 sm:flex-row sm:justify-end',
        className
      )}
      {...props}
    />
  );
}
