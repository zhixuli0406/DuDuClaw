import { describe, it, expect } from 'vitest';
import { mergeComment, upsertTask } from './tasks-store';
import type { TaskComment, TaskInfo } from '@/lib/api';

function comment(id: string, at: string, body = ''): TaskComment {
  return { id, task_id: 't1', author_user: 'u1', body, created_at: at };
}

function task(id: string, over: Partial<TaskInfo> = {}): TaskInfo {
  return {
    id,
    title: `Task ${id}`,
    description: '',
    status: 'todo',
    priority: 'medium',
    assigned_to: '',
    created_by: 'user',
    created_at: '2026-07-18T00:00:00Z',
    updated_at: '2026-07-18T00:00:00Z',
    tags: [],
    ...over,
  };
}

describe('upsertTask (task-board dedup, #2)', () => {
  it('appends a task whose id is not present', () => {
    const existing = [task('a')];
    const next = upsertTask(existing, task('b'));
    expect(next.map((t) => t.id)).toEqual(['a', 'b']);
  });

  it('replaces in place instead of duplicating when the id already exists', () => {
    // Models optimistic createTask add followed by the task.created WS echo.
    const optimistic = [task('a'), task('dup', { title: 'first' })];
    const next = upsertTask(optimistic, task('dup', { title: 'from-ws' }));
    expect(next).toHaveLength(2);
    expect(next.filter((t) => t.id === 'dup')).toHaveLength(1);
    expect(next.find((t) => t.id === 'dup')?.title).toBe('from-ws');
  });

  it('preserves order when replacing', () => {
    const existing = [task('a'), task('b'), task('c')];
    const next = upsertTask(existing, task('b', { status: 'done' }));
    expect(next.map((t) => t.id)).toEqual(['a', 'b', 'c']);
    expect(next[1].status).toBe('done');
  });
});

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
