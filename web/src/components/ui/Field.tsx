import { useId, type ReactNode } from 'react';
import { cn } from '@/lib/utils';

/** Shared control classes — reuse for raw <input>/<select>/<textarea>. */
export const controlClass =
  'h-9 w-full rounded-lg border border-[var(--panel-border)] bg-[var(--panel-fill)] px-3 text-sm ' +
  'text-stone-800 placeholder:text-stone-400 focus-visible:border-amber-500/50 focus-visible:outline-none ' +
  'focus-visible:ring-2 focus-visible:ring-amber-500/30 dark:text-stone-100';

/**
 * Field — label + control + help/error wrapper for forms. Pass the control as
 * children; the htmlFor/id wiring is automatic via `htmlFor`.
 */
export function Field({
  label,
  htmlFor,
  help,
  error,
  required,
  children,
  className,
}: {
  label?: ReactNode;
  htmlFor?: string;
  help?: ReactNode;
  error?: ReactNode;
  required?: boolean;
  children: ReactNode;
  className?: string;
}) {
  const autoId = useId();
  const id = htmlFor ?? autoId;
  return (
    <div className={cn('space-y-1.5', className)}>
      {label && (
        <label htmlFor={id} className="block text-xs font-medium text-stone-600 dark:text-stone-300">
          {label}
          {required && <span className="ml-0.5 text-rose-500">*</span>}
        </label>
      )}
      {children}
      {error ? (
        <p className="text-xs text-rose-600 dark:text-rose-400">{error}</p>
      ) : help ? (
        <p className="text-xs text-stone-400 dark:text-stone-500">{help}</p>
      ) : null}
    </div>
  );
}
