import { forwardRef, type TextareaHTMLAttributes } from 'react';
import { cn } from '@/lib/utils';

/** Textarea — MDS multiline field, styled to match Input (spec §4 Input). */
export const Textarea = forwardRef<
  HTMLTextAreaElement,
  TextareaHTMLAttributes<HTMLTextAreaElement>
>(({ className, ...props }, ref) => (
  <textarea
    ref={ref}
    data-slot="textarea"
    className={cn(
      'min-h-16 w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1.5 text-base outline-none md:text-sm',
      'field-sizing-content placeholder:text-muted-foreground',
      'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
      'disabled:pointer-events-none disabled:opacity-50',
      'aria-invalid:border-destructive',
      'dark:bg-input/30',
      className
    )}
    {...props}
  />
));
Textarea.displayName = 'Textarea';
