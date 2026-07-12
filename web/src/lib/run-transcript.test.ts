import { describe, expect, it } from 'vitest';
import {
  cardsForEvents,
  isRunLive,
  relativeParts,
  runDurationSecs,
  runStatusMeta,
  type RunEvent,
} from './run-transcript';

const NOW = Date.parse('2026-07-11T09:00:00Z');

describe('cardsForEvents', () => {
  it('maps text and tool_use events to prose / tool cards in order', () => {
    const events: RunEvent[] = [
      { kind: 'text', role: 'user', ts: '2026-07-11T08:00:00Z', preview: '幫我查 SOP' },
      {
        kind: 'tool_use',
        tool: 'shared_wiki_read',
        ok: true,
        ts: '2026-07-11T08:00:05Z',
        preview: 'path=sop/deploy',
      },
      { kind: 'text', role: 'assistant', ts: '2026-07-11T08:00:30Z', preview: '找到了…' },
    ];
    const cards = cardsForEvents(events);
    expect(cards).toHaveLength(3);
    expect(cards[0]).toMatchObject({ type: 'prose', role: 'user', text: '幫我查 SOP' });
    expect(cards[1]).toMatchObject({ type: 'tool', tool: 'shared_wiki_read', ok: true });
    expect(cards[2]).toMatchObject({ type: 'prose', role: 'assistant' });
  });

  it('maps persisted tool_step and todo_update events to step / todo cards', () => {
    const events: RunEvent[] = [
      {
        kind: 'tool_step',
        label: 'Read',
        seq: 1,
        ts: '2026-07-11T08:00:01Z',
        preview: 'src/lib.rs',
      },
      {
        kind: 'todo_update',
        label: '2/5',
        seq: 2,
        ts: '2026-07-11T08:00:02Z',
        preview: '📋 任務進度(2/5 完成)',
      },
    ];
    const cards = cardsForEvents(events);
    expect(cards).toHaveLength(2);
    expect(cards[0]).toMatchObject({ type: 'step', tool: 'Read', preview: 'src/lib.rs' });
    expect(cards[1]).toMatchObject({ type: 'todo', label: '2/5' });
  });

  it('drops unknown kinds and malformed events instead of fabricating cards', () => {
    const events: RunEvent[] = [
      // future/unknown kind — must not render as anything.
      { kind: 'thinking_summary', ts: '2026-07-11T08:00:00Z', preview: '…' },
      // text without a valid role.
      { kind: 'text', ts: '2026-07-11T08:00:01Z', preview: 'x' },
      // tool_use without a tool name.
      { kind: 'tool_use', ts: '2026-07-11T08:00:02Z', preview: 'y' },
      // persisted kinds without their required label.
      { kind: 'tool_step', ts: '2026-07-11T08:00:03Z', preview: 'z' },
      { kind: 'todo_update', ts: '2026-07-11T08:00:04Z', preview: '{}' },
    ];
    expect(cardsForEvents(events)).toHaveLength(0);
  });

  it('treats a missing ok flag as success and false as failure', () => {
    const cards = cardsForEvents([
      { kind: 'tool_use', tool: 'a', ts: 't', preview: '' },
      { kind: 'tool_use', tool: 'b', ok: false, ts: 't', preview: '' },
    ]);
    expect(cards[0]).toMatchObject({ type: 'tool', ok: true });
    expect(cards[1]).toMatchObject({ type: 'tool', ok: false });
  });
});

describe('run status helpers', () => {
  it('isRunLive is true only while running', () => {
    expect(isRunLive({ status: 'running' })).toBe(true);
    expect(isRunLive({ status: 'completed' })).toBe(false);
    expect(isRunLive({ status: 'no_reply' })).toBe(false);
  });

  it('runStatusMeta maps every status to an i18n id + status token', () => {
    expect(runStatusMeta('running').labelId).toBe('runs.status.running');
    expect(runStatusMeta('completed').token).toBe('var(--status-task-done)');
    // Unknown statuses fall back to the no-reply bucket (never crash).
    expect(runStatusMeta('weird').labelId).toBe('runs.status.no_reply');
  });
});

describe('relativeParts', () => {
  it('picks minutes / hours / days by magnitude', () => {
    expect(relativeParts('2026-07-11T08:58:00Z', NOW)).toEqual({ value: -2, unit: 'minute' });
    expect(relativeParts('2026-07-11T04:00:00Z', NOW)).toEqual({ value: -5, unit: 'hour' });
    expect(relativeParts('2026-07-08T09:00:00Z', NOW)).toEqual({ value: -3, unit: 'day' });
  });

  it('returns null for unparseable timestamps', () => {
    expect(relativeParts('not-a-time', NOW)).toBeNull();
  });
});

describe('runDurationSecs', () => {
  it('computes whole-second durations for finished runs only', () => {
    expect(
      runDurationSecs({ started_at: '2026-07-11T08:00:00Z', ended_at: '2026-07-11T08:01:30Z' }),
    ).toBe(90);
    expect(runDurationSecs({ started_at: '2026-07-11T08:00:00Z', ended_at: null })).toBeNull();
    // Never a negative duration.
    expect(
      runDurationSecs({ started_at: '2026-07-11T09:00:00Z', ended_at: '2026-07-11T08:00:00Z' }),
    ).toBeNull();
  });
});
