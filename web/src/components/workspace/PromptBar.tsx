import { useRef, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { Send, Loader2 } from 'lucide-react';
import { isImeComposing } from '@/lib/keyboard';
import { cn } from '@/lib/utils';

/**
 * The large composer used by the 交辦 panel (`AssignSheet`, UX plan I-1b): one
 * rounded input with a control row underneath.
 *
 * Originally written against `useChatStore` for a workspace landing that never
 * shipped (zero importers until 2026-08-15). Rewritten as a **controlled,
 * presentational** composer so the assign panel owns the draft and the caller
 * decides what "submit" means — `/chat` in 問一問 mode, `tasks.goal_create` in
 * 交辦 mode. Attachment/voice affordances were dropped rather than carried
 * over: `/chat` has its own attachment pipeline (`WebChatPage`), and
 * `goal_create` has no attachment channel at all, so an attach button here
 * would be a promise the assign path cannot keep.
 */
export function PromptBar({
  value,
  onChange,
  onSubmit,
  placeholder,
  label,
  submitLabel,
  submitting = false,
  disabled = false,
  showSubmit = true,
  rows = 3,
  controls,
  id = 'assign-prompt',
}: {
  value: string;
  onChange: (next: string) => void;
  onSubmit: () => void;
  placeholder?: string;
  label?: string;
  submitLabel?: string;
  submitting?: boolean;
  disabled?: boolean;
  /** Hide the inline send button when the caller owns the primary CTA (the
   *  assign panel puts it in the dialog footer). Enter still submits. */
  showSubmit?: boolean;
  rows?: number;
  /** Control row rendered to the left of the send button. */
  controls?: ReactNode;
  id?: string;
}) {
  const intl = useIntl();
  const inputRef = useRef<HTMLTextAreaElement>(null);

  const canSend = value.trim().length > 0 && !submitting && !disabled;

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Skip Enter while a CJK IME is composing — the first Enter confirms
    // candidate selection, not send. See `isImeComposing`.
    if (e.key === 'Enter' && !e.shiftKey && !isImeComposing(e)) {
      e.preventDefault();
      if (canSend) onSubmit();
    }
  };

  return (
    <div
      className={cn(
        'rounded-2xl border border-surface-border bg-surface px-4 pb-3 pt-4 shadow-[var(--surface-shadow)]',
        disabled && 'opacity-70'
      )}
    >
      <label htmlFor={id} className="sr-only">
        {label ?? intl.formatMessage({ id: 'workspace.promptLabel' })}
      </label>
      <textarea
        id={id}
        ref={inputRef}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={handleKeyDown}
        rows={rows}
        placeholder={placeholder ?? intl.formatMessage({ id: 'workspace.promptPlaceholder' })}
        className="w-full resize-none bg-transparent px-1 text-base text-foreground placeholder:text-muted-foreground focus-visible:outline-none"
        disabled={disabled}
      />

      {/* Control row */}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        {controls}

        {showSubmit && (
          <>
            <div className="flex-1" />
            <button
              type="button"
              onClick={onSubmit}
              disabled={!canSend}
              aria-label={submitLabel ?? intl.formatMessage({ id: 'workspace.send' })}
              className="flex h-9 shrink-0 items-center gap-1.5 rounded-lg bg-brand px-3 text-sm font-medium text-brand-foreground transition-colors outline-none hover:bg-brand/90 focus-visible:ring-3 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50"
            >
              {submitting ? <Loader2 className="h-4 w-4 animate-spin" /> : <Send className="h-4 w-4" />}
              <span>{submitLabel ?? intl.formatMessage({ id: 'workspace.send' })}</span>
            </button>
          </>
        )}
      </div>
    </div>
  );
}
