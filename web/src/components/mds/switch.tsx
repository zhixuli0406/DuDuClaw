import { forwardRef, type ComponentPropsWithoutRef } from 'react';
import { Switch as BaseSwitch } from '@base-ui/react/switch';
import { cn } from '@/lib/utils';

/** Switch — MDS toggle (spec §5.3 SettingsRow controls), built on @base-ui/react. */
export const Switch = forwardRef<
  HTMLButtonElement,
  ComponentPropsWithoutRef<typeof BaseSwitch.Root>
>(({ className, ...props }, ref) => (
  <BaseSwitch.Root
    ref={ref}
    data-slot="switch"
    className={cn(
      'inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full border border-transparent p-0.5 outline-none transition-colors',
      // Off state keeps a muted-foreground outline: the `input` fill alone is
      // about 1.5:1 against dark surfaces (WCAG 1.4.11 asks for 3:1).
      'data-[unchecked]:border-muted-foreground data-[unchecked]:bg-input data-[checked]:bg-primary',
      'focus-visible:ring-3 focus-visible:ring-ring/50',
      'data-[disabled]:cursor-not-allowed data-[disabled]:opacity-50',
      className
    )}
    {...props}
  >
    <BaseSwitch.Thumb
      data-slot="switch-thumb"
      className={cn(
        'pointer-events-none block size-4 rounded-full bg-background shadow-sm transition-transform',
        'data-[unchecked]:translate-x-0 data-[checked]:translate-x-4'
      )}
    />
  </BaseSwitch.Root>
));
Switch.displayName = 'Switch';
