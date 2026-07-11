import { useEffect, useRef, useState } from 'react';
import { cn } from '@/lib/utils';

/**
 * InlineEditor — click-to-edit text (paperclip P2 title/description). Shows the
 * value as static text; clicking (or focusing) swaps in an input/textarea.
 *
 * Commit/cancel rules:
 *  - single-line: Enter commits, Esc cancels, blur commits.
 *  - multiline:   ⌘/Ctrl+Enter commits, Esc cancels, blur commits.
 * An empty value after trim is rejected (reverts) so titles can't be blanked.
 */
export function InlineEditor({
  value,
  onCommit,
  multiline = false,
  placeholder,
  ariaLabel,
  readOnly = false,
  className,
  textClassName,
}: {
  value: string;
  onCommit: (next: string) => void;
  multiline?: boolean;
  placeholder?: string;
  ariaLabel?: string;
  readOnly?: boolean;
  className?: string;
  /** Extra classes for the resting text (e.g. heading size). */
  textClassName?: string;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement | HTMLTextAreaElement>(null);

  // Keep the draft in sync when the upstream value changes while not editing.
  useEffect(() => {
    if (!editing) setDraft(value);
  }, [value, editing]);

  useEffect(() => {
    if (editing && inputRef.current) {
      inputRef.current.focus();
      const el = inputRef.current;
      el.setSelectionRange(el.value.length, el.value.length);
    }
  }, [editing]);

  const commit = () => {
    const trimmed = draft.trim();
    setEditing(false);
    if (trimmed && trimmed !== value) onCommit(trimmed);
    else setDraft(value); // reject empty / no-op
  };
  const cancel = () => {
    setDraft(value);
    setEditing(false);
  };

  if (readOnly || !editing) {
    return (
      <button
        type="button"
        disabled={readOnly}
        onClick={() => !readOnly && setEditing(true)}
        aria-label={ariaLabel}
        className={cn(
          'w-full rounded-control px-1.5 py-1 text-left',
          !readOnly && 'hover:bg-stone-500/8 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50 dark:hover:bg-white/5',
          className,
        )}
      >
        <span className={cn(!value && 'text-stone-400 dark:text-stone-500', textClassName)}>
          {value || placeholder || ''}
        </span>
      </button>
    );
  }

  const shared =
    'w-full resize-none rounded-control border border-[var(--panel-border-strong)] bg-[var(--panel-fill)] px-1.5 py-1 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-500/50';

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      cancel();
    } else if (e.key === 'Enter' && (!multiline || e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      commit();
    }
  };

  return multiline ? (
    <textarea
      ref={inputRef as React.RefObject<HTMLTextAreaElement>}
      value={draft}
      rows={3}
      placeholder={placeholder}
      aria-label={ariaLabel}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={onKeyDown}
      className={cn(shared, textClassName, className)}
    />
  ) : (
    <input
      ref={inputRef as React.RefObject<HTMLInputElement>}
      value={draft}
      placeholder={placeholder}
      aria-label={ariaLabel}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={onKeyDown}
      className={cn(shared, textClassName, className)}
    />
  );
}
