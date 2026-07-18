import { type ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * Segmented — MDS segmented control (spec §4 Segmented). Options-driven,
 * controlled. Used for time-range / series toggles on report pages.
 */
export type SegmentedOption<T extends string> = {
  value: T;
  label: ReactNode;
  disabled?: boolean;
};

export function Segmented<T extends string>({
  value,
  onValueChange,
  options,
  className,
  'aria-label': ariaLabel,
}: {
  value: T;
  onValueChange: (value: T) => void;
  options: readonly SegmentedOption<T>[];
  className?: string;
  'aria-label'?: string;
}) {
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      data-slot="segmented"
      className={cn(
        'inline-flex gap-0.5 rounded-md bg-muted p-0.5',
        className
      )}
    >
      {options.map((opt) => {
        const selected = opt.value === value;
        return (
          <button
            key={opt.value}
            type="button"
            role="radio"
            aria-checked={selected}
            disabled={opt.disabled}
            data-slot="segmented-item"
            data-state={selected ? 'active' : 'inactive'}
            onClick={() => onValueChange(opt.value)}
            className={cn(
              'rounded-sm px-2.5 py-1 text-xs font-medium text-muted-foreground outline-none transition-colors',
              'focus-visible:ring-3 focus-visible:ring-ring/50',
              'disabled:pointer-events-none disabled:opacity-50',
              selected
                ? 'bg-background text-foreground shadow-sm'
                : 'hover:text-foreground'
            )}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
