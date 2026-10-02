import { describe, it, expect } from 'vitest';
import type { TaskStatusKey } from '@/components/ui';
import {
  BACKEND_STATUSES,
  FORWARD_LOOKING_STATUSES,
  isBackendStatus,
  toStatusKey,
  toBackendStatus,
} from './task-status';

const ALL_KEYS: readonly TaskStatusKey[] = [
  'backlog',
  'todo',
  'in_progress',
  'in_review',
  'done',
  'blocked',
  'cancelled',
];

describe('task-status mapping layer', () => {
  it('round-trips every backend status through the UI key vocabulary', () => {
    for (const s of BACKEND_STATUSES) {
      const key = toStatusKey(s);
      expect(key).toBe(s); // identity on the shared 4 names
      expect(toBackendStatus(key)).toBe(s);
    }
  });

  it('classifies exactly the 4 backend statuses as writable', () => {
    const writable = ALL_KEYS.filter(isBackendStatus);
    expect(writable.sort()).toEqual([...BACKEND_STATUSES].sort());
  });

  it('returns null for forward-looking statuses (never coerced to a real write)', () => {
    for (const key of FORWARD_LOOKING_STATUSES) {
      expect(isBackendStatus(key)).toBe(false);
      expect(toBackendStatus(key)).toBeNull();
    }
  });

  it('partitions the full 7-state set into writable + forward-looking with no overlap', () => {
    const writable = ALL_KEYS.filter(isBackendStatus);
    const forward = ALL_KEYS.filter((k) => !isBackendStatus(k));
    expect(writable.length + forward.length).toBe(ALL_KEYS.length);
    expect(forward.sort()).toEqual([...FORWARD_LOOKING_STATUSES].sort());
    // No key is both.
    expect(writable.some((k) => (FORWARD_LOOKING_STATUSES as readonly string[]).includes(k))).toBe(false);
  });

  it('toBackendStatus is exhaustive over TaskStatus (compile-time subset guarantee)', () => {
    // Sanity: the four writable statuses round-trip through toBackendStatus.
    const sample: TaskStatusKey[] = ['todo', 'in_progress', 'done', 'blocked'];
    expect(sample.every((s) => toBackendStatus(s) === s)).toBe(true);
  });
});

describe('toStatusKey is total (L2 round 4 — discovery statuses crashed Home)', () => {
  it('maps the discovery waiting states onto the "not started" glyph', () => {
    expect(toStatusKey('pending_approval')).toBe('todo');
    expect(toStatusKey('queued')).toBe('todo');
  });
  it('maps any status it does not recognise to a safe key, never passes it through', () => {
    expect(toStatusKey('some_future_state' as never)).toBe('todo');
    expect(toStatusKey('' as never)).toBe('todo');
  });
  it('never makes the new statuses writable', () => {
    expect(BACKEND_STATUSES).not.toContain('pending_approval');
    expect(BACKEND_STATUSES).not.toContain('queued');
  });
});

describe('toStatusKey tolerates null / undefined at runtime (L2 round 7)', () => {
  it('maps both to the safe key', () => {
    expect(toStatusKey(null as never)).toBe('todo');
    expect(toStatusKey(undefined as never)).toBe('todo');
  });
});
