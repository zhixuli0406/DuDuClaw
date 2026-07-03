import type { ButtonHTMLAttributes, ComponentType, ReactNode } from 'react';
import { Loader2 } from 'lucide-react';
import { cn } from '@/lib/utils';

type Variant = 'primary' | 'secondary' | 'ghost' | 'danger';
type Size = 'sm' | 'md';

const base =
  'inline-flex items-center justify-center gap-1.5 rounded-lg font-medium transition-colors ' +
  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 ' +
  'disabled:pointer-events-none disabled:opacity-50';

const variants: Record<Variant, string> = {
  primary:
    'bg-amber-500 text-white shadow-sm hover:bg-amber-600 active:bg-amber-700',
  secondary:
    'border border-[var(--panel-border-strong)] bg-[var(--panel-fill)] text-stone-700 ' +
    'hover:bg-[var(--panel-fill-hover)] dark:text-stone-200',
  ghost:
    'text-stone-600 hover:bg-stone-500/10 hover:text-stone-900 ' +
    'dark:text-stone-300 dark:hover:bg-white/5 dark:hover:text-stone-100',
  danger:
    'bg-rose-500 text-white hover:bg-rose-600 active:bg-rose-700',
};

const sizes: Record<Size, string> = {
  sm: 'h-8 px-3 text-xs',
  md: 'h-9 px-4 text-sm',
};

export function Button({
  variant = 'secondary',
  size = 'md',
  icon: Icon,
  iconRight: IconRight,
  pending = false,
  disabled,
  children,
  className,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: Variant;
  size?: Size;
  icon?: ComponentType<{ className?: string }>;
  iconRight?: ComponentType<{ className?: string }>;
  /** In-flight state: swaps the leading icon for a spinner, disables + aria-busy. */
  pending?: boolean;
  children?: ReactNode;
}) {
  const iconOnly = !children;
  return (
    <button
      className={cn(
        base,
        variants[variant],
        sizes[size],
        iconOnly && (size === 'sm' ? 'w-8 px-0' : 'w-9 px-0'),
        className
      )}
      disabled={disabled || pending}
      aria-busy={pending || undefined}
      {...props}
    >
      {pending ? (
        <Loader2 className="h-4 w-4 shrink-0 animate-spin" aria-hidden="true" />
      ) : (
        Icon && <Icon className="h-4 w-4 shrink-0" />
      )}
      {children}
      {IconRight && <IconRight className="h-4 w-4 shrink-0" />}
    </button>
  );
}
