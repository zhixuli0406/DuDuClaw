import { forwardRef, type InputHTMLAttributes } from 'react';
import { cn } from '@/lib/utils';

/** Input — MDS text field primitive (spec §4 Input). */
export const Input = forwardRef<
  HTMLInputElement,
  InputHTMLAttributes<HTMLInputElement>
>(({ className, type = 'text', ...props }, ref) => (
  <input
    ref={ref}
    type={type}
    data-slot="input"
    className={cn(
      'h-8 w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 text-base outline-none md:text-sm',
      'placeholder:text-muted-foreground',
      'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
      'disabled:pointer-events-none disabled:opacity-50',
      'aria-invalid:border-destructive',
      'dark:bg-input/30',
      className
    )}
    {...props}
  />
));
Input.displayName = 'Input';
