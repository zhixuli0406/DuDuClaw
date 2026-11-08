/**
 * Pure state helpers for the `/skills/new` 4-step wizard (T13.1). Extracted from
 * the React component so the step machine + backend-status alignment is unit
 * testable in isolation (no DOM). See `wizard-machine.test.ts`.
 *
 * ── Wizard step ↔ backend status alignment ──────────────────────────────────
 *  describe : create (→ status `draft`) then generate (→ status `generating`)
 *  generate : poll `custom_list`; the record advances `generating → draft` ONLY
 *             if the authoring agent's completion flips it. As of the W5-BE
 *             backend nothing auto-fires that transition, so the wizard also
 *             allows a manual proceed — the true completion gate is `submit`,
 *             which reads the isolated draft file and errors if it is missing.
 *  form     : `custom_update` (human fields; status unchanged)
 *  review   : `custom_submit` (→ `pending_approval`, or an error frame on
 *             high/critical risk — fail-closed, surfaced verbatim).
 */
import type { DuduFace } from '@/components/mascot/faces';
import type { CustomSkillStatus } from '@/lib/api-custom-skills';

export type WizardStep = 'describe' | 'generate' | 'form' | 'review';

/** The four steps in order. */
export const WIZARD_STEPS: readonly WizardStep[] = ['describe', 'generate', 'form', 'review'];

/** DuDu's face for each step (curious → writing → idle → proud, per spec). */
export const WIZARD_FACE: Record<WizardStep, DuduFace> = {
  describe: 'curious',
  generate: 'writing',
  form: 'idle',
  review: 'proud',
};

/** i18n key stem for each step's short label. */
export const WIZARD_STEP_LABEL: Record<WizardStep, string> = {
  describe: 'skills.new.step.describe',
  generate: 'skills.new.step.generate',
  form: 'skills.new.step.form',
  review: 'skills.new.step.review',
};

/** Zero-based index of a step. */
export function stepIndex(step: WizardStep): number {
  return WIZARD_STEPS.indexOf(step);
}

/** The step after `step`, or `step` itself when already last. */
export function nextStep(step: WizardStep): WizardStep {
  const i = stepIndex(step);
  return i < 0 || i >= WIZARD_STEPS.length - 1 ? step : WIZARD_STEPS[i + 1];
}

/** The step before `step`, or `step` itself when already first. */
export function prevStep(step: WizardStep): WizardStep {
  const i = stepIndex(step);
  return i <= 0 ? step : WIZARD_STEPS[i - 1];
}

/** True once `a` is at or past `b` — drives the stepper's done/active styling. */
export function isStepReached(current: WizardStep, target: WizardStep): boolean {
  return stepIndex(current) >= stepIndex(target);
}

/**
 * Whether the describe step is complete enough to create + generate:
 * a non-empty description and a chosen builder agent.
 */
export function canStartGeneration(description: string, builderAgent: string): boolean {
  return description.trim().length > 0 && builderAgent.trim().length > 0;
}

/** Coarse generation phase derived from the record status while on step 2. */
export type GenerationPhase = 'working' | 'ready' | 'failed' | 'unexpected';

/**
 * `generationError` is the record's `generation_error`: a `draft` carrying one
 * means the last attempt failed (the backend has no separate failed status).
 */
export function generationPhase(
  status: CustomSkillStatus | undefined,
  generationError?: string | null,
): GenerationPhase {
  switch (status) {
    case 'generating':
      return 'working';
    case 'draft':
      return generationError ? 'failed' : 'ready';
    // A prior rejection sent it back — the human can proceed to fill the form.
    case 'rejected':
      return 'ready';
    case undefined:
      return 'working';
    default:
      // pending_approval / approved / retired are unexpected mid-wizard.
      return 'unexpected';
  }
}

/**
 * Whether the human may leave the generate step for the form step. Auto-true
 * once the backend reports a draft; otherwise gated on an explicit human
 * confirmation (backend gives no completion callback — honest fallback).
 */
export function canProceedFromGenerate(
  status: CustomSkillStatus | undefined,
  draftConfirmed: boolean,
  generationError?: string | null,
): boolean {
  const phase = generationPhase(status, generationError);
  // A failed generation has no draft: never allow proceeding (or confirming).
  if (phase === 'failed') return false;
  return phase === 'ready' || draftConfirmed;
}

/** Poll backoff: 4s → 8s → 16s … capped at 60s. `attempt` is 0-based. */
export const POLL_BASE_MS = 4000;
export const POLL_MAX_MS = 60000;
export function pollDelay(attempt: number): number {
  return Math.min(POLL_BASE_MS * 2 ** Math.max(0, attempt), POLL_MAX_MS);
}

/** Statuses worth polling for (an in-flight backend operation). */
export function isPollableStatus(status: CustomSkillStatus | undefined): boolean {
  return status === 'generating' || status === 'pending_approval';
}

/**
 * "Taking longer than usual": a generating record the SERVER says may be
 * regenerated (`can_regenerate`, computed against the server's stale window and
 * clock — never the browser clock).
 */
export function isGenerationSlow(
  status: CustomSkillStatus | undefined,
  canRegenerate: boolean | null | undefined,
): boolean {
  return status === 'generating' && canRegenerate === true;
}

const DISPATCH_ERROR_LABELS = new Set([
  'enqueue',
  'stopped',
  'budget',
  'autonomy_paused',
  'sandbox',
  'binary_missing',
  'auth_failed',
  'rate_limited',
  'billing',
  'timeout',
  'spawn_error',
  'empty_response',
  'no_accounts',
  'cooling_down',
  'unknown',
]);

/**
 * Map a backend `generation_error` reason code (`timeout`, `no_draft`,
 * `in_progress`, `dispatch:<label>`) to an i18n message id. Anything
 * unrecognised (including legacy free-text errors stored by older builds) maps
 * to a generic message — raw backend text is never shown.
 */
export function generationErrorMessageId(code: string | null | undefined): string {
  const c = (code ?? '').trim();
  if (c === 'timeout' || c === 'no_draft' || c === 'in_progress') {
    return `skills.generationError.${c}`;
  }
  if (c.startsWith('dispatch:')) {
    const label = c.slice('dispatch:'.length);
    return DISPATCH_ERROR_LABELS.has(label)
      ? `skills.generationError.dispatch.${label}`
      : 'skills.generationError.dispatch.unknown';
  }
  return 'skills.generationError.unknown';
}

/** True when an RPC rejection is the backend's "generation already running" code. */
export function isInProgressError(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : typeof e === 'string' ? e : '';
  return /(^|[^a-z_])in_progress($|[^a-z_])/.test(msg);
}

/** The form step requires a non-empty display name (defaulted from the draft). */
export function canProceedFromForm(displayName: string): boolean {
  return displayName.trim().length > 0;
}
