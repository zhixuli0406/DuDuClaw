import { useEffect } from 'react';
import { createPortal } from 'react-dom';
import { CharacterAvatar } from '@/components/ui';
import { TASK_DONE_XP } from './task-celebrate';

/**
 * TaskDoneBurst — the transient §5.5 flourish that rides on top of the global
 * confetti: the assignee's character strikes a `celebrating` pose for ~3s while
 * a "+12 XP" chip floats up. Purely visual (the XP number mirrors the gateway
 * growth engine's `完成任務 +12`, it does not itself award anything).
 *
 * Reduced-motion: the confetti is already suppressed upstream by `celebrate()`;
 * here the CSS float animations are stilled by the global reduced-motion rule,
 * so the chip simply appears and fades on the timer instead of drifting.
 */
export function TaskDoneBurst({
  agentId,
  agentName,
  onDone,
}: {
  agentId: string;
  agentName?: string;
  onDone: () => void;
}) {
  useEffect(() => {
    const t = window.setTimeout(onDone, 3000);
    return () => window.clearTimeout(t);
  }, [onDone]);

  if (typeof document === 'undefined') return null;

  return createPortal(
    <div
      aria-hidden="true"
      className="pointer-events-none fixed inset-0 z-[190] grid place-items-center"
    >
      <div className="animate-badge-pop flex flex-col items-center gap-2">
        <CharacterAvatar agentId={agentId} name={agentName} size={96} variant="bust" pose="celebrating" emote="celebrating" />
        <span className="animate-fade-up rounded-full bg-[color:var(--xp)] px-3 py-1 text-sm font-semibold text-white shadow-[var(--shadow-pop)]">
          +{TASK_DONE_XP} XP
        </span>
      </div>
    </div>,
    document.body,
  );
}
