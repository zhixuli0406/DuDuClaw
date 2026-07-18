import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/**
 * SpeechBubble — a soft rounded speech balloon with a little tail, for
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
      ? 'bg-brand/12 text-brand ring-brand/25'
      : 'bg-surface text-foreground ring-surface-border';

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
        'relative inline-block max-w-xs rounded-2xl px-3 py-2 text-sm ring-1 ring-inset shadow-[var(--surface-shadow)]',
        toneCls,
        className,
      )}
    >
      {children}
      <span
        aria-hidden="true"
        className={cn(
          'absolute h-2 w-2 rotate-45 rounded-[2px]',
          tone === 'accent' ? 'bg-brand/12' : 'bg-surface',
          tailPos[side],
        )}
      />
    </div>
  );
}
