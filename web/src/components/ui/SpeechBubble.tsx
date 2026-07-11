import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * SpeechBubble — a soft `rounded-bubble` speech balloon with a little tail, for
 * character utterances (DuDu / agent avatars, world chat, empty-state poses).
 * openhuman "head-top chat bubble" language, Soft Play tokens.
 */
export function SpeechBubble({
  children,
  side = 'bottom',
  tone = 'neutral',
  className,
}: {
  children: ReactNode;
  /** Which edge the tail points from (toward the speaker). */
  side?: 'top' | 'bottom' | 'left' | 'right';
  tone?: 'neutral' | 'accent';
  className?: string;
}) {
  const toneCls =
    tone === 'accent'
      ? 'bg-amber-500/12 text-amber-900 ring-amber-500/25 dark:text-amber-200'
      : 'bg-[var(--panel-fill)] text-stone-700 ring-[var(--panel-border)] dark:text-stone-200';

  // Tail is a rotated square tucked under the bubble on the chosen side.
  const tailPos: Record<string, string> = {
    bottom: 'left-4 -bottom-1',
    top: 'left-4 -top-1',
    left: '-left-1 top-4',
    right: '-right-1 top-4',
  };

  return (
    <div
      className={cn(
        'relative inline-block max-w-xs rounded-bubble px-3 py-2 text-sm ring-1 ring-inset shadow-[var(--shadow-soft)]',
        toneCls,
        className,
      )}
    >
      {children}
      <span
        aria-hidden="true"
        className={cn(
          'absolute h-2 w-2 rotate-45 rounded-[2px]',
          tone === 'accent' ? 'bg-amber-500/12' : 'bg-[var(--panel-fill)]',
          tailPos[side],
        )}
      />
    </div>
  );
}
