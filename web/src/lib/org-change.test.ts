import { describe, expect, it } from 'vitest';
import { descendantsOf, orgErrorCode, pendingStatus, pickDropTarget, wouldCycle, type NodeRect } from './org-change';
import type { OrgPendingChange } from './api';

const agents = [
  { name: 'dudu' },
  { name: 'sales', reports_to: 'dudu' },
  { name: 'quote', reports_to: 'dudu' },
  { name: 'track', reports_to: 'sales' },
  { name: 'deep', reports_to: 'track' },
];

describe('org change helpers', () => {
  it('reads the code of a structured RPC error and ignores plain errors', () => {
    expect(orgErrorCode({ code: 'cycle', message: 'x' })).toBe('cycle');
    expect(orgErrorCode(new Error('boom'))).toBeNull();
    expect(orgErrorCode('permission')).toBeNull();
  });

  it('finds every descendant and flags a reporting cycle', () => {
    expect([...descendantsOf(agents, 'sales')].sort()).toEqual(['deep', 'track']);
    expect(wouldCycle(agents, 'sales', 'deep')).toBe(true);
    expect(wouldCycle(agents, 'sales', 'sales')).toBe(true);
    expect(wouldCycle(agents, 'sales', 'quote')).toBe(false);
    expect(wouldCycle(agents, 'sales', '')).toBe(false);
  });

  it('survives an existing loop in the data', () => {
    const loop = [{ name: 'a', reports_to: 'b' }, { name: 'b', reports_to: 'a' }];
    expect(descendantsOf(loop, 'a')).toEqual(new Set(['b']));
  });

  it('marks a request made against a moved placement as needing a new request', () => {
    const base = { approval_id: 'x', agent_id: 'quote', before: { reports_to: '', department: '' }, after: { reports_to: 'sales', department: '' },
      base: 'h', proposer: 'u', proposer_email: '', reason: '', created_at: '', expires_at: null } as OrgPendingChange;
    expect(pendingStatus({ ...base, stale: false })).toBe('pending');
    expect(pendingStatus({ ...base, stale: true })).toBe('needs_resubmit');
  });
});

describe('pickDropTarget', () => {
  const rects: NodeRect[] = [
    { name: 'dudu', x: 0, y: 0, w: 100, h: 50 },
    { name: 'sales', x: 200, y: 0, w: 100, h: 50 },
    { name: 'quote', x: 0, y: 100, w: 100, h: 50 },
    { name: 'track', x: 200, y: 100, w: 100, h: 50 },
  ];

  it('returns the node under the pointer', () => {
    expect(pickDropTarget(rects, { x: 250, y: 25 }, 'quote', agents)).toBe('sales');
  });

  it('returns null over empty space', () => {
    expect(pickDropTarget(rects, { x: 150, y: 75 }, 'quote', agents)).toBeNull();
  });

  it('rejects the dragged node, its current manager and anything below it', () => {
    expect(pickDropTarget(rects, { x: 50, y: 125 }, 'quote', agents)).toBeNull(); // itself
    expect(pickDropTarget(rects, { x: 50, y: 25 }, 'quote', agents)).toBeNull(); // current manager
    expect(pickDropTarget(rects, { x: 250, y: 125 }, 'sales', agents)).toBeNull(); // its own report
  });
});
