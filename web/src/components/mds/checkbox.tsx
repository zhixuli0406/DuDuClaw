import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Checkbox as BaseCheckbox } from '@base-ui/react/checkbox';
import { CheckIcon, MinusIcon } from 'lucide-react';
import { cn } from '@/lib/utils';

/** Checkbox — MDS checkbox (spec §5.4 list-view controls), built on @base-ui/react. */
export const Checkbox = forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<typeof BaseCheckbox.Root>
>(({ className, ...props }, ref) => (
  <BaseCheckbox.Root
    ref={ref}
    data-slot="checkbox"
    className={cn(
      'group peer size-4 shrink-0 rounded-[4px] border border-input bg-transparent outline-none transition-colors',
      'data-[checked]:border-primary data-[checked]:bg-primary data-[checked]:text-primary-foreground',
      'data-[indeterminate]:border-primary data-[indeterminate]:bg-primary data-[indeterminate]:text-primary-foreground',
      'focus-visible:ring-3 focus-visible:ring-ring/50',
      'data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50',
      'aria-invalid:border-destructive dark:bg-input/30',
      className
    )}
    {...props}
  >
    <BaseCheckbox.Indicator
      data-slot="checkbox-indicator"
      className="flex items-center justify-center text-current"
    >
      <CheckIcon className="size-3.5 group-data-[indeterminate]:hidden" />
      <MinusIcon className="hidden size-3.5 group-data-[indeterminate]:block" />
    </BaseCheckbox.Indicator>
  </BaseCheckbox.Root>
));
Checkbox.displayName = 'Checkbox';
