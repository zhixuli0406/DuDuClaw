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

  it('canProceedFromGenerate: auto when ready, else needs a human confirm', () => {
    expect(canProceedFromGenerate('generating', false)).toBe(false);
    expect(canProceedFromGenerate('generating', true)).toBe(true);
    expect(canProceedFromGenerate('draft', false)).toBe(true);
  });
});
