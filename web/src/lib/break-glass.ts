/**
 * v1.81 P1 break-glass ("需要再驗證身分"): an Admin who is not the owner
 * reads a Personal employee's private data, changes its configuration, or
 * decides one of its approval cards. The gateway refuses the first attempt
 * with its step-up-required message; this helper then asks the Admin to
 * confirm their identity (StepUpDialog via BreakGlassPromptHost) and retries
 * the SAME call once with the proof's token in `step_up_csrf`.
 *
 * - No prompt host mounted (tests, a page rendered alone): the original
 *   refusal is rethrown, so the page shows it as before.
 * - Cancelled: `BreakGlassCancelled` is thrown; the caller treats it like
 *   the refusal (nothing was read or changed).
 * - A token is remembered per `scope` (an employee id) for a few minutes so
 *   one page's several reads do not ask again; the server still audits and
 *   checks every request, and a refusal drops the remembered token.
 */
import type { StepUpToken } from './step-up';
import { lookupMessage } from './toast';
import { setStepUpRetryHandler } from './step-up-retry-registry';

export type BreakGlassPurpose = 'read' | 'write' | 'decide';

export interface BreakGlassRequest {
  purpose: BreakGlassPurpose;
}

type Prompt = (request: BreakGlassRequest) => Promise<StepUpToken | null>;

let prompt: Prompt | null = null;

/** Mount point: `BreakGlassPromptHost` registers its dialog here. */
export function registerBreakGlassPrompt(fn: Prompt): () => void {
  prompt = fn;
  return () => {
    if (prompt === fn) prompt = null;
  };
}

/** Thrown when the Admin closes the prompt. Its message is plain language
 *  (the gateway's refusal text names a request field). */
export class BreakGlassCancelled extends Error {
  readonly code = 'BREAK_GLASS_CANCELLED';
  constructor() {
    super(lookupMessage('restoreFlow.breakGlass.cancelled'));
    this.name = 'BreakGlassCancelled';
  }
}

/** Both gateway texts (`personal_data_gate::STEP_UP_REQUIRED`,
 *  `personal_decide_gate::DECIDE_STEP_UP_REQUIRED`) name the request field;
 *  a structured `{ code: 'STEP_UP_REQUIRED' }` is accepted too. */
export function isBreakGlassRefusal(err: unknown): boolean {
  if (err == null) return false;
  if (typeof err === 'object') {
    const e = err as { code?: unknown; message?: unknown };
    if (e.code === 'STEP_UP_REQUIRED' || e.code === 'BREAK_GLASS_REQUIRED') return true;
    if (typeof e.message === 'string') return e.message.includes('step_up_csrf');
    return false;
  }
  return typeof err === 'string' && err.includes('step_up_csrf');
}

/** Remembered well inside the gateway's five-minute proof window. */
const REMEMBER_MS = 4 * 60 * 1000;
/** After a cancel, a background reload does not ask again right away. */
const CANCEL_QUIET_MS = 60 * 1000;

const remembered = new Map<string, { token: StepUpToken; until: number }>();
const quietUntil = new Map<string, number>();

export function forgetBreakGlass(scope?: string): void {
  if (scope === undefined) {
    remembered.clear();
    quietUntil.clear();
  } else {
    remembered.delete(scope);
    quietUntil.delete(scope);
  }
}

export interface BreakGlassOptions {
  purpose: BreakGlassPurpose;
  /** Employee id the token may be reused for. Omit for one-off decisions. */
  scope?: string;
  /** False for background refreshes: reuse a remembered token, never ask. */
  interactive?: boolean;
}

/** Number of explicit `withBreakGlass` calls in flight. While > 0 the generic
 *  RPC-layer retry stays out of the way, so a call site that opted in never
 *  prompts twice (a concurrent unrelated call merely falls back to the old
 *  behaviour: its refusal is shown). */
let explicitDepth = 0;

const READ_METHOD = /(^|[._])(list|get|read|history|status|search|view|show|info|tail|export)([._]|$)/;

/** Generic layer (installed on the RPC client below): prompt once, retry once
 *  with the token. Cancelled / no host / not a refusal → the ORIGINAL error. */
async function genericStepUpRetry(
  err: unknown,
  method: string,
  retry: (token: StepUpToken) => Promise<unknown>,
): Promise<unknown> {
  if (explicitDepth > 0 || !isBreakGlassRefusal(err) || !prompt) throw err;
  const quiet = quietUntil.get(GENERIC_SCOPE) ?? 0;
  if (quiet > Date.now()) throw err;
  const token = await prompt({ purpose: READ_METHOD.test(method) ? 'read' : 'write' });
  if (!token) {
    quietUntil.set(GENERIC_SCOPE, Date.now() + CANCEL_QUIET_MS);
    throw err;
  }
  quietUntil.delete(GENERIC_SCOPE);
  return retry(token);
}

const GENERIC_SCOPE = '\u0000generic';

/** Install the generic RPC-layer retry. Called by the prompt host on mount (no
 *  host mounted → no generic retry, the original error surfaces). Returns the
 *  uninstall function. */
export function installStepUpRetry(): () => void {
  setStepUpRetryHandler(genericStepUpRetry);
  return () => setStepUpRetryHandler(null);
}

export async function withBreakGlass<T>(
  run: (token?: StepUpToken) => Promise<T>,
  options: BreakGlassOptions,
): Promise<T> {
  explicitDepth++;
  try {
    return await withBreakGlassInner(run, options);
  } finally {
    explicitDepth--;
  }
}

async function withBreakGlassInner<T>(
  run: (token?: StepUpToken) => Promise<T>,
  { purpose, scope, interactive = true }: BreakGlassOptions,
): Promise<T> {
  const now = Date.now();
  const saved = scope ? remembered.get(scope) : undefined;
  const reuse = saved && saved.until > now ? saved.token : undefined;
  try {
    return await run(reuse);
  } catch (err) {
    if (!isBreakGlassRefusal(err)) throw err;
    if (scope) remembered.delete(scope);
    const quiet = scope ? (quietUntil.get(scope) ?? 0) : 0;
    if (!interactive || !prompt || quiet > Date.now()) throw err;
    const token = await prompt({ purpose });
    if (!token) {
      if (scope) quietUntil.set(scope, Date.now() + CANCEL_QUIET_MS);
      throw new BreakGlassCancelled();
    }
    if (scope) {
      remembered.set(scope, { token, until: Date.now() + REMEMBER_MS });
      quietUntil.delete(scope);
    }
    try {
      return await run(token);
    } catch (again) {
      if (scope && isBreakGlassRefusal(again)) remembered.delete(scope);
      throw again;
    }
  }
}
