import { useEffect, useRef, useState, type HTMLAttributes } from 'react';
import { cn } from '@/lib/utils';

/**
 * Spinner — MDS text spinner rendered with braille glyphs in a monospace
 * (fixed-width) cell so it never reflows (spec §4 UnicodeSpinner). Honours
 * `prefers-reduced-motion` by holding a single static frame.
 */
const FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'] as const;

function prefersReducedMotion(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  );
}

export function Spinner({
  className,
  intervalMs = 80,
  label = 'Loading',
  ...props
}: HTMLAttributes<HTMLSpanElement> & {
  intervalMs?: number;
  label?: string;
}) {
  const [frame, setFrame] = useState(0);
  const reduced = useRef(prefersReducedMotion());

  useEffect(() => {
    if (reduced.current) return;
    const id = setInterval(
      () => setFrame((f) => (f + 1) % FRAMES.length),
      intervalMs
    );
    return () => clearInterval(id);
  }, [intervalMs]);

  return (
    <span
      role="status"
      aria-label={label}
      data-slot="spinner"
      className={cn(
        'inline-block w-[1ch] text-center font-mono tabular-nums select-none',
        className
      )}
      {...props}
    >
      {FRAMES[frame]}
    </span>
  );
}
