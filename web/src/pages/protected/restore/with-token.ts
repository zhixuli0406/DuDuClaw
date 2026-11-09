import type { StepUpToken } from '@/lib/step-up';

/** The Admin closed the step-up dialog: nothing was sent. */
export class StepUpCancelledError extends Error {
  readonly code = 'STEP_UP_CANCELLED';
  constructor() {
    super('STEP_UP_CANCELLED');
    this.name = 'StepUpCancelledError';
  }
}

export function isStepUpCancelled(err: unknown): boolean {
  return err instanceof StepUpCancelledError;
}

/** Runs an Admin write with a fresh step-up token (asks only when needed). */
export type WithToken = <T>(fn: (token: StepUpToken) => Promise<T>) => Promise<T>;

export function makeWithToken(ensure: () => Promise<StepUpToken | null>): WithToken {
  return async <T>(fn: (token: StepUpToken) => Promise<T>) => {
    const token = await ensure();
    if (!token) throw new StepUpCancelledError();
    return fn(token);
  };
}
