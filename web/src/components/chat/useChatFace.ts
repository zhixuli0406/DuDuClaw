import { useEffect, useState } from 'react';
import type { DuduFace } from '@/components/mascot';
import type { ChatPhase } from '@/stores/chat-store';

/**
 * Derive DuDu's face for the conversation stage (V7 / T7.1) from the live turn
 * phase plus two page-local signals. The mapping is presentation-only; the store
 * owns the phase truth.
 *
 * Timed flourishes handled here (the store stays timer-free for the face):
 *  - entrance wave: on first mount of an empty conversation, DuDu waves for a
 *    beat, then settles to idle.
 *  - post-turn happy: when a reply completes (`phase === 'done'`), DuDu beams for
 *    2s before falling back to idle.
 *
 * Precedence: error → thinking → speaking → happy(done) → waving(entrance) →
 * listening(typing) → idle.
 */
export function useChatFace(
  phase: ChatPhase,
  isTyping: boolean,
  hasMessages: boolean,
): DuduFace {
  // Wave only when the conversation starts empty (nothing to catch up on).
  const [entrance, setEntrance] = useState(!hasMessages);
  const [happy, setHappy] = useState(false);

  useEffect(() => {
    if (!entrance) return;
    const t = setTimeout(() => setEntrance(false), 1600);
    return () => clearTimeout(t);
  }, [entrance]);

  useEffect(() => {
    if (phase !== 'done') {
      setHappy(false);
      return;
    }
    setHappy(true);
    const t = setTimeout(() => setHappy(false), 2000);
    return () => clearTimeout(t);
  }, [phase]);

  if (phase === 'error') return 'concerned';
  if (phase === 'thinking') return 'thinking';
  if (phase === 'speaking') return 'speaking';
  if (phase === 'done' && happy) return 'happy';
  if (entrance) return 'waving';
  if (isTyping) return 'listening';
  return 'idle';
}
