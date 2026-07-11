import { useEffect, useState } from 'react';
import { cn } from '@/lib/utils';
import { controlClass } from '@/components/ui';

/** cents → display string in major units, e.g. 1250 → "12.50". */
export function centsToDisplay(cents: number): string {
  if (!Number.isFinite(cents)) return '';
  return (cents / 100).toFixed(2);
}

/** major-unit input → integer cents, e.g. "12.5" → 1250. Empty → 0. */
export function displayToCents(input: string): number {
  const trimmed = input.trim();
  if (trimmed === '') return 0;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || n < 0) return 0;
  return Math.round(n * 100);
}

/**
 * MoneyField — budget input shown in whole currency units ($ 12.50) but stored
 * as integer cents. Every budget/cost field uses this so users never type raw
 * cents. Keeps a local display buffer so partial input (e.g. "12.") is not
 * clobbered mid-type; commits cents on change and normalises on blur.
 */
export function MoneyField({
  cents,
  onChange,
  symbol = '$',
  disabled,
  className,
  id,
}: {
  cents: number;
  onChange: (cents: number) => void;
  symbol?: string;
  disabled?: boolean;
  className?: string;
  id?: string;
}) {
  const [buffer, setBuffer] = useState(() => centsToDisplay(cents));

  // Re-sync display when the stored value changes from outside (load / reset),
  // but not while the user is mid-edit producing the same cents value.
  useEffect(() => {
    if (displayToCents(buffer) !== cents) setBuffer(centsToDisplay(cents));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cents]);

  return (
    <div className={cn('relative', className)}>
      <span className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-sm text-stone-400">
        {symbol}
      </span>
      <input
        id={id}
        type="number"
        min={0}
        step="0.01"
        inputMode="decimal"
        disabled={disabled}
        value={buffer}
        onChange={(e) => {
          setBuffer(e.target.value);
          onChange(displayToCents(e.target.value));
        }}
        onBlur={() => setBuffer(centsToDisplay(displayToCents(buffer)))}
        className={cn(controlClass, 'pl-7')}
      />
    </div>
  );
}
