import { useEffect, useRef, useState } from 'react';
import { installStepUpRetry, registerBreakGlassPrompt, type BreakGlassRequest } from '@/lib/break-glass';
import type { StepUpToken } from '@/lib/step-up';
import { StepUpDialog } from './StepUpDialog';

/**
 * Mounted once (App root). Answers `withBreakGlass` prompts with the shared
 * StepUpDialog. A second prompt while one is open waits for the same answer
 * instead of stacking dialogs.
 */
export function BreakGlassPromptHost() {
  const [request, setRequest] = useState<BreakGlassRequest | null>(null);
  const waiting = useRef<Array<(token: StepUpToken | null) => void>>([]);

  useEffect(
    () =>
      registerBreakGlassPrompt(
        (next) =>
          new Promise<StepUpToken | null>((resolve) => {
            waiting.current.push(resolve);
            setRequest((open) => open ?? next);
          }),
      ),
    [],
  );

  // The generic RPC-layer step-up retry exists only while a prompt host is mounted.
  useEffect(() => installStepUpRetry(), []);

  // Unmounting answers every waiter with "cancelled".
  useEffect(() => () => {
    for (const resolve of waiting.current.splice(0)) resolve(null);
  }, []);

  const finish = (token: StepUpToken | null) => {
    for (const resolve of waiting.current.splice(0)) resolve(token);
    setRequest(null);
  };

  return <StepUpDialog open={request !== null} purpose={request?.purpose ?? 'read'} onDone={finish} />;
}
