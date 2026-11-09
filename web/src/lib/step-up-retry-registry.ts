/** Holds the generic step-up retry handler. Kept in its own module (no imports)
 *  so `break-glass.ts` does not depend on `ws-client.ts` exports, which many
 *  tests replace with partial `vi.mock` factories. */

/** Installed by `lib/break-glass.ts`: decides whether a failed call is a
 *  step-up refusal and, if so, prompts and calls `retry(token)` once. It must
 *  rethrow `err` when it does not retry. */
export type StepUpRetryHandler = (
  err: unknown,
  method: string,
  retry: (token: number[]) => Promise<unknown>,
) => Promise<unknown>;

let handler: StepUpRetryHandler | null = null;

export function setStepUpRetryHandler(h: StepUpRetryHandler | null): void {
  handler = h;
}

export function getStepUpRetryHandler(): StepUpRetryHandler | null {
  return handler;
}
