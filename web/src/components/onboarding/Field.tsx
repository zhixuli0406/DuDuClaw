import { useId, type ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * Onboarding form primitives (LoginPage / WelcomePage / OnboardWizardPage).
 * These replace the Calm Glass `ui/Field` + `controlClass` on the onboarding
 * surface with MDS-token styling (spec §4 Input / §5.8). They live outside
 * `components/mds/` because they are onboarding-specific compositions, not core
 * design-system primitives.
 */

/** Raw-control class mirroring the MDS <Input> — for native <select>/<input>
 *  where a component wrapper is impractical (spec §4 Input). */
export const fieldControl =
  'h-8 w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 text-base outline-none md:text-sm ' +
  'placeholder:text-muted-foreground ' +
  'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 ' +
  'disabled:pointer-events-none disabled:opacity-50 aria-invalid:border-destructive ' +
  'dark:bg-input/30';

/** Vertical label + control + help/error wrapper. Pass the control as children;
 *  supply `htmlFor` to wire the label to a control with a matching `id`. */
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
        <label htmlFor={id} className="block text-xs font-medium text-foreground">
          {label}
          {required && <span className="ml-0.5 text-destructive">*</span>}
        </label>
      )}
      {children}
      {error ? (
        <p className="text-xs text-destructive">{error}</p>
      ) : help ? (
        <p className="text-xs text-muted-foreground">{help}</p>
      ) : null}
    </div>
  );
}
