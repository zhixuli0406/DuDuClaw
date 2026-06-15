import { useEffect, useRef, type ReactNode } from 'react';
import { X } from 'lucide-react';
import { cn } from '@/lib/utils';

interface DialogProps {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  className?: string;
}

export function Dialog({ open, onClose, title, children, className }: DialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    if (open && !el.open) {
      el.showModal();
    } else if (!open && el.open) {
      el.close();
    }
  }, [open]);

  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    const handleClose = () => onClose();
    el.addEventListener('close', handleClose);
    return () => el.removeEventListener('close', handleClose);
  }, [onClose]);

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby="dialog-title"
      aria-modal="true"
      className={cn(
        'glass-overlay w-full max-w-lg rounded-2xl p-0 text-stone-900 backdrop:bg-stone-950/45 backdrop:backdrop-blur-md dark:text-stone-100',
        className
      )}
    >
      <div className="flex items-center justify-between border-b border-stone-300/40 px-6 py-4 dark:border-white/8">
        <h3 id="dialog-title" className="text-lg font-semibold tracking-tight text-stone-900 dark:text-stone-50">{title}</h3>
        <button
          onClick={onClose}
          className="rounded-lg p-1.5 text-stone-400 hover:bg-stone-500/10 hover:text-stone-600 dark:hover:text-stone-300"
        >
          <X className="h-4 w-4" />
        </button>
      </div>
      <div className="px-6 py-5">{children}</div>
    </dialog>
  );
}

interface FormFieldProps {
  label: string;
  children: ReactNode;
  hint?: string;
  htmlFor?: string;
}

export function FormField({ label, children, hint, htmlFor }: FormFieldProps) {
  return (
    <div className="space-y-1.5">
      <label htmlFor={htmlFor} className="block text-sm font-medium text-stone-700 dark:text-stone-300">
        {label}
      </label>
      {children}
      {hint && <p className="text-xs text-stone-400 dark:text-stone-500">{hint}</p>}
    </div>
  );
}

export const inputClass =
  'w-full rounded-lg border border-stone-300/70 bg-white/60 px-3 py-2 text-sm text-stone-900 backdrop-blur placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-2 focus:ring-amber-500/25 dark:border-white/10 dark:bg-white/5 dark:text-stone-50 dark:placeholder:text-stone-500 dark:focus:border-amber-400';

export const selectClass = inputClass;

export const buttonPrimary =
  'inline-flex items-center justify-center gap-2 rounded-lg bg-gradient-to-b from-amber-400 to-amber-500 px-4 py-2 text-sm font-medium text-white shadow-[0_4px_14px_-4px_rgba(245,158,11,0.55),inset_0_1px_0_0_rgba(255,255,255,0.3)] transition-all hover:from-amber-400 hover:to-amber-600 active:scale-[0.98] disabled:opacity-50';

export const buttonSecondary =
  'inline-flex items-center justify-center gap-2 rounded-lg border border-stone-300/70 bg-white/50 px-4 py-2 text-sm font-medium text-stone-700 backdrop-blur transition-colors hover:bg-white/80 dark:border-white/10 dark:bg-white/5 dark:text-stone-300 dark:hover:bg-white/10';
