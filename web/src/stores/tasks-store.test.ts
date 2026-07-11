import { describe, it, expect } from 'vitest';
import { mergeComment } from './tasks-store';
import type { TaskComment } from '@/lib/api';

function comment(id: string, at: string, body = ''): TaskComment {
  return { id, task_id: 't1', author_user: 'u1', body, created_at: at };
}

describe('mergeComment (L2 task comments)', () => {
  it('appends a new comment and keeps the list oldest-first', () => {
    const existing = [comment('c1', '2026-07-10T10:00:00Z')];
    const merged = mergeComment(existing, comment('c2', '2026-07-10T10:05:00Z'));
    expect(merged.map((c) => c.id)).toEqual(['c1', 'c2']);
  });

  it('inserts an out-of-order comment into chronological position', () => {
    const existing = [comment('c2', '2026-07-10T10:05:00Z')];
    const merged = mergeComment(existing, comment('c1', '2026-07-10T10:00:00Z'));
    expect(merged.map((c) => c.id)).toEqual(['c1', 'c2']);
  });

  it('is idempotent on the same id (optimistic insert vs broadcast echo)', () => {
    const existing = [comment('c1', '2026-07-10T10:00:00Z')];
    const merged = mergeComment(existing, comment('c1', '2026-07-10T10:00:00Z'));
    expect(merged).toBe(existing);
    expect(merged).toHaveLength(1);
  });
});
