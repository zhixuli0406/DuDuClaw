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
        'w-full max-w-lg rounded-xl border border-stone-200 bg-white p-0 shadow-xl backdrop:bg-black/40 backdrop:backdrop-blur-sm dark:border-stone-700 dark:bg-stone-900',
        className
      )}
    >
      <div className="flex items-center justify-between border-b border-stone-200 px-6 py-4 dark:border-stone-700">
        <h3 id="dialog-title" className="text-lg font-semibold text-stone-900 dark:text-stone-50">{title}</h3>
        <button
          onClick={onClose}
          className="rounded-lg p-1.5 text-stone-400 hover:bg-stone-100 hover:text-stone-600 dark:hover:bg-stone-800 dark:hover:text-stone-300"
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
}

export function FormField({ label, children, hint }: FormFieldProps) {
  return (
    <div className="space-y-1.5">
      <label className="block text-sm font-medium text-stone-700 dark:text-stone-300">
        {label}
      </label>
      {children}
      {hint && <p className="text-xs text-stone-400 dark:text-stone-500">{hint}</p>}
    </div>
  );
}

export const inputClass =
  'w-full rounded-lg border border-stone-300 bg-white px-3 py-2 text-sm text-stone-900 placeholder:text-stone-400 focus:border-amber-500 focus:outline-none focus:ring-2 focus:ring-amber-500/20 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-50 dark:placeholder:text-stone-500 dark:focus:border-amber-400';

export const selectClass = inputClass;

export const buttonPrimary =
  'inline-flex items-center justify-center gap-2 rounded-lg bg-amber-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-600 disabled:opacity-50';

export const buttonSecondary =
  'inline-flex items-center justify-center gap-2 rounded-lg border border-stone-300 bg-white px-4 py-2 text-sm font-medium text-stone-700 transition-colors hover:bg-stone-50 dark:border-stone-600 dark:bg-stone-800 dark:text-stone-300 dark:hover:bg-stone-700';
