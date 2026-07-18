import { useEffect, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { ChevronDown } from 'lucide-react';
import { cn } from '@/lib/utils';

const STORAGE_PREFIX = 'dudu.settings.advanced.';

function readOpen(storageKey: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(STORAGE_PREFIX + storageKey);
    return v === null ? fallback : v === '1';
  } catch {
    return fallback;
  }
}

/**
 * AdvancedSection — the single, site-wide "show advanced settings" disclosure.
 * Everyday controls stay visible; engineering/technical knobs live inside this
 * collapsible block. Open/closed state is remembered per `storageKey` in
 * localStorage so a user who opened Advanced on one page keeps it open there.
 */
export function AdvancedSection({
  storageKey,
  label,
  defaultOpen = false,
  children,
  className,
}: {
  storageKey: string;
  label?: ReactNode;
  defaultOpen?: boolean;
  children: ReactNode;
  className?: string;
}) {
  const intl = useIntl();
  const [open, setOpen] = useState(() => readOpen(storageKey, defaultOpen));

  // Re-sync when the key changes (e.g. a per-agent section switching agents).
  useEffect(() => {
    setOpen(readOpen(storageKey, defaultOpen));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [storageKey]);

  const toggle = () => {
    setOpen((prev) => {
      const next = !prev;
      try {
        localStorage.setItem(STORAGE_PREFIX + storageKey, next ? '1' : '0');
      } catch {
        /* private mode / quota — non-fatal, just don't persist */
      }
      return next;
    });
  };

  return (
    <div className={cn('border-t border-surface-border pt-3', className)}>
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground"
      >
        <ChevronDown className={cn('h-3.5 w-3.5 transition-transform', !open && '-rotate-90')} />
        {label ?? intl.formatMessage({ id: 'settings.advanced' })}
      </button>
      {open && <div className="mt-4 space-y-4">{children}</div>}
    </div>
  );
}
