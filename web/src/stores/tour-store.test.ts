import { describe, it, expect, beforeEach } from 'vitest';
import '@/test/mocks';
import { useTourStore } from './tour-store';
import { useAuthStore } from './auth-store';

function setUser(id: string) {
  useAuthStore.setState({
    user: { id, email: `${id}@local`, display_name: id, role: 'admin' },
  } as never);
}

beforeEach(() => {
  localStorage.clear();
  setUser('u1');
  useTourStore.setState({
    status: 'unset',
    stepIndex: 0,
    promptPending: false,
    hydratedFor: null,
  });
});

describe('tour-store', () => {
  it('requestPrompt only arms the prompt when the tour is unset', () => {
    useTourStore.getState().requestPrompt();
    expect(useTourStore.getState().promptPending).toBe(true);

    useTourStore.setState({ status: 'completed', promptPending: false });
    useTourStore.getState().requestPrompt();
    expect(useTourStore.getState().promptPending).toBe(false);
  });

  it('start runs the tour from step 0 and clears the prompt', () => {
    useTourStore.setState({ promptPending: true, stepIndex: 3 });
    useTourStore.getState().start();
    const s = useTourStore.getState();
    expect(s.status).toBe('running');
    expect(s.stepIndex).toBe(0);
    expect(s.promptPending).toBe(false);
  });

  it('next/back move the step index without going negative', () => {
    const { next, back } = useTourStore.getState();
    next();
    next();
    expect(useTourStore.getState().stepIndex).toBe(2);
    back();
    expect(useTourStore.getState().stepIndex).toBe(1);
    back();
    back();
    expect(useTourStore.getState().stepIndex).toBe(0);
  });

  it('skip persists a terminal state (show-once)', () => {
    useTourStore.getState().skip();
    expect(useTourStore.getState().status).toBe('skipped');
    expect(localStorage.getItem('ddc.tour.v1.u1')).toBe('skipped');
  });

  it('finish persists completed', () => {
    useTourStore.getState().finish();
    expect(useTourStore.getState().status).toBe('completed');
    expect(localStorage.getItem('ddc.tour.v1.u1')).toBe('completed');
  });

  it('dismissPrompt marks skipped so it never nags again', () => {
    useTourStore.setState({ promptPending: true });
    useTourStore.getState().dismissPrompt();
    expect(useTourStore.getState().promptPending).toBe(false);
    expect(useTourStore.getState().status).toBe('skipped');
    expect(localStorage.getItem('ddc.tour.v1.u1')).toBe('skipped');
  });

  it('hydrate restores a persisted terminal state for the current user', () => {
    localStorage.setItem('ddc.tour.v1.u1', 'completed');
    useTourStore.getState().hydrate();
    expect(useTourStore.getState().status).toBe('completed');
    expect(useTourStore.getState().hydratedFor).toBe('u1');
  });

  it('hydrate is per-user — a fresh user starts unset', () => {
    localStorage.setItem('ddc.tour.v1.u1', 'completed');
    setUser('u2');
    useTourStore.setState({ hydratedFor: null, status: 'unset' });
    useTourStore.getState().hydrate();
    expect(useTourStore.getState().status).toBe('unset');
  });
});
