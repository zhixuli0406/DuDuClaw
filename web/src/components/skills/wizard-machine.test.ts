import { describe, it, expect } from 'vitest';
import {
  WIZARD_STEPS,
  WIZARD_FACE,
  stepIndex,
  nextStep,
  prevStep,
  isStepReached,
  canStartGeneration,
  canProceedFromGenerate,
  canProceedFromForm,
  generationPhase,
  pollDelay,
  isPollableStatus,
  isGenerationSlow,
  generationErrorMessageId,
  isInProgressError,
} from './wizard-machine';

describe('wizard-machine steps', () => {
  it('has four steps in order', () => {
    expect(WIZARD_STEPS).toEqual(['describe', 'generate', 'form', 'review']);
  });

  it('maps a DuDu face per step (curious → writing → idle → proud)', () => {
    expect(WIZARD_FACE).toEqual({
      describe: 'curious',
      generate: 'writing',
      form: 'idle',
      review: 'proud',
    });
  });

  it('stepIndex / next / prev respect bounds', () => {
    expect(stepIndex('describe')).toBe(0);
    expect(stepIndex('review')).toBe(3);
    expect(nextStep('describe')).toBe('generate');
    expect(nextStep('review')).toBe('review'); // clamped
    expect(prevStep('generate')).toBe('describe');
    expect(prevStep('describe')).toBe('describe'); // clamped
  });

  it('isStepReached compares by position', () => {
    expect(isStepReached('form', 'describe')).toBe(true);
    expect(isStepReached('form', 'form')).toBe(true);
    expect(isStepReached('form', 'review')).toBe(false);
  });
});

describe('wizard-machine guards', () => {
  it('canStartGeneration needs a description and a builder', () => {
    expect(canStartGeneration('do a thing', 'agent-a')).toBe(true);
    expect(canStartGeneration('   ', 'agent-a')).toBe(false);
    expect(canStartGeneration('do a thing', '')).toBe(false);
  });

  it('canProceedFromForm needs a non-empty display name', () => {
    expect(canProceedFromForm('My skill')).toBe(true);
    expect(canProceedFromForm('  ')).toBe(false);
  });
});

describe('generation phase ↔ backend status alignment', () => {
  it('generating is still working', () => {
    expect(generationPhase('generating')).toBe('working');
    expect(generationPhase(undefined)).toBe('working');
  });

  it('draft (or a bounce-back to draft via rejected) means ready', () => {
    expect(generationPhase('draft')).toBe('ready');
    expect(generationPhase('rejected')).toBe('ready');
  });

  it('terminal/mid states are unexpected mid-wizard', () => {
    expect(generationPhase('pending_approval')).toBe('unexpected');
    expect(generationPhase('approved')).toBe('unexpected');
    expect(generationPhase('retired')).toBe('unexpected');
  });

  it('a draft carrying a generation error is the failed phase', () => {
    expect(generationPhase('draft', 'boom')).toBe('failed');
    expect(generationPhase('draft', null)).toBe('ready');
    expect(generationPhase('draft', '')).toBe('ready');
    // The error only matters on draft.
    expect(generationPhase('generating', 'old')).toBe('working');
  });

  it('a failed generation can never proceed, even if "confirmed"', () => {
    expect(canProceedFromGenerate('draft', true, 'boom')).toBe(false);
    expect(canProceedFromGenerate('draft', false, null)).toBe(true);
  });

  it('pollDelay backs off 4s → 8s → 16s … capped at 60s', () => {
    expect([0, 1, 2, 3, 4, 5, 10].map(pollDelay)).toEqual([4000, 8000, 16000, 32000, 60000, 60000, 60000]);
  });

  it('only generating / pending_approval are pollable', () => {
    expect(isPollableStatus('generating')).toBe(true);
    expect(isPollableStatus('pending_approval')).toBe(true);
    expect(isPollableStatus('draft')).toBe(false);
    expect(isPollableStatus('approved')).toBe(false);
    expect(isPollableStatus(undefined)).toBe(false);
  });

  it('isGenerationSlow follows the server-computed can_regenerate, not a clock', () => {
    expect(isGenerationSlow('generating', true)).toBe(true);
    expect(isGenerationSlow('generating', false)).toBe(false);
    expect(isGenerationSlow('generating', undefined)).toBe(false);
    expect(isGenerationSlow('generating', null)).toBe(false);
    // Only meaningful while generating.
    expect(isGenerationSlow('draft', true)).toBe(false);
    expect(isGenerationSlow(undefined, true)).toBe(false);
  });

  it('maps backend reason codes to i18n ids (never raw text)', () => {
    expect(generationErrorMessageId('timeout')).toBe('skills.generationError.timeout');
    expect(generationErrorMessageId('no_draft')).toBe('skills.generationError.no_draft');
    expect(generationErrorMessageId('in_progress')).toBe('skills.generationError.in_progress');
    expect(generationErrorMessageId('dispatch:rate_limited')).toBe('skills.generationError.dispatch.rate_limited');
    expect(generationErrorMessageId('dispatch:made_up')).toBe('skills.generationError.dispatch.unknown');
    // Legacy free-text errors from older builds fall back to the generic message.
    expect(generationErrorMessageId('The assistant is unavailable')).toBe('skills.generationError.unknown');
    expect(generationErrorMessageId(null)).toBe('skills.generationError.unknown');
  });

  it('isInProgressError recognises the in_progress rejection', () => {
    expect(isInProgressError(new Error('in_progress'))).toBe(true);
    expect(isInProgressError(new Error('RPC error: in_progress'))).toBe(true);
    expect(isInProgressError(new Error('cannot generate from status retired'))).toBe(false);
    expect(isInProgressError(undefined)).toBe(false);
  });

  it('canProceedFromGenerate: auto when ready, else needs a human confirm', () => {
    expect(canProceedFromGenerate('generating', false)).toBe(false);
    expect(canProceedFromGenerate('generating', true)).toBe(true);
    expect(canProceedFromGenerate('draft', false)).toBe(true);
  });
});
