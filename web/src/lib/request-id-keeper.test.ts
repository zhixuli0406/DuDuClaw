import { describe, expect, it } from 'vitest';
import { createRequestIdKeeper, serverAnswered } from './request-id-keeper';

describe('createRequestIdKeeper', () => {
  it('reuses the id for a retry and makes a new one after an answer', () => {
    let n = 0;
    const keeper = createRequestIdKeeper(() => `id-${++n}`);
    expect(keeper.take('routine-a')).toBe('id-1');
    // The first press failed without an answer: the retry keeps the id.
    expect(keeper.take('routine-a')).toBe('id-1');
    expect(keeper.take('routine-b')).toBe('id-2');
    keeper.settle('routine-a');
    expect(keeper.take('routine-a')).toBe('id-3');
  });

  it('tells a server answer from a transport failure', () => {
    expect(serverAnswered('routine is not active')).toBe(true);
    expect(serverAnswered({ code: 'workflow_invalid', message: 'x' })).toBe(true);
    expect(serverAnswered(new Error('Request timeout: cron.run_now'))).toBe(false);
    expect(serverAnswered(new Error('Connection lost'))).toBe(false);
    expect(serverAnswered(undefined)).toBe(false);
  });
});
