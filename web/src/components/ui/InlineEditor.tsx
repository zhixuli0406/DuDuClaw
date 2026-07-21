import { useEffect, useRef, useState } from 'react';
import { cn } from '@/lib/utils';
import { isImeComposing } from '@/lib/keyboard';

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
          'w-full rounded-xl px-1.5 py-1 text-left',
          !readOnly && 'outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50',
          className,
        )}
      >
        <span className={cn(!value && 'text-muted-foreground', textClassName)}>
          {value || placeholder || ''}
        </span>
      </button>
    );
  }

  const shared =
    'w-full resize-none rounded-lg border border-input bg-transparent px-1.5 py-1 outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30';

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      cancel();
    } else if (e.key === 'Enter' && !isImeComposing(e) && (!multiline || e.metaKey || e.ctrlKey)) {
      // Skip commit while a CJK IME is composing — Enter confirms candidates.
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
