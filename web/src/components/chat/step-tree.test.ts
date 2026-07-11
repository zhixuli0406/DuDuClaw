import { describe, it, expect } from 'vitest';
import { applyStep, type StepNode } from '@/stores/chat-store';

/** Fold a sequence of frames from an empty tree. */
function fold(frames: Parameters<typeof applyStep>[1][]): readonly StepNode[] {
  return frames.reduce<readonly StepNode[]>((tree, f) => applyStep(tree, f), []);
}

describe('applyStep (tool step tree reducer)', () => {
  it('opens a running node on start', () => {
    const tree = fold([{ phase: 'start', tool: 'Read', summary: '/etc/hosts', depth: 0 }]);
    expect(tree).toHaveLength(1);
    expect(tree[0]).toMatchObject({ tool: 'Read', summary: '/etc/hosts', depth: 0, running: true });
  });

  it('closes the matching node on end', () => {
    const tree = fold([
      { phase: 'start', tool: 'Bash', summary: 'cargo test', depth: 0 },
      { phase: 'end', tool: 'Bash', depth: 0 },
    ]);
    expect(tree).toHaveLength(1);
    expect(tree[0].running).toBe(false);
  });

  it('nests by depth and closes the most-recent same-tool node (LIFO)', () => {
    const tree = fold([
      { phase: 'start', tool: 'Task', depth: 0 },
      { phase: 'start', tool: 'Bash', summary: 'a', depth: 1 },
      { phase: 'start', tool: 'Bash', summary: 'b', depth: 1 },
      { phase: 'end', tool: 'Bash', depth: 1 }, // closes 'b' (the latest open Bash)
    ]);
    expect(tree.map((n) => n.running)).toEqual([true, true, false]);
    expect(tree[1].summary).toBe('a'); // still running
    expect(tree[2].summary).toBe('b'); // closed
  });

  it('ignores an orphan end with no matching open node (never corrupts the tree)', () => {
    const before = fold([{ phase: 'start', tool: 'Read', depth: 0 }]);
    const after = applyStep(before, { phase: 'end', tool: 'Grep', depth: 0 });
    expect(after).toBe(before); // unchanged reference — no-op
  });

  it('leaves an unmatched start running (honest still-working signal)', () => {
    const tree = fold([
      { phase: 'start', tool: 'Read', depth: 0 },
      { phase: 'end', tool: 'Read', depth: 0 },
      { phase: 'start', tool: 'Edit', depth: 0 }, // never ends
    ]);
    expect(tree[0].running).toBe(false);
    expect(tree[1].running).toBe(true);
  });

  it('ignores unknown phases', () => {
    const before = fold([{ phase: 'start', tool: 'Read', depth: 0 }]);
    const after = applyStep(before, { phase: 'weird', tool: 'Read' });
    expect(after).toBe(before);
  });

  it('defaults a missing depth to 0', () => {
    const tree = fold([{ phase: 'start', tool: 'Read' }]);
    expect(tree[0].depth).toBe(0);
  });
});
