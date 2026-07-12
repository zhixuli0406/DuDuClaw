import { describe, it, expect, beforeEach } from 'vitest';
import {
  approvalRisk,
  riskTone,
  riskNeedsConfirm,
  extractPlanFacts,
  hasPlanFacts,
  countByKind,
  similarBatches,
  isoDay,
  dailyRollover,
  readApprovedToday,
  bumpApprovedToday,
  UNKNOWN_KIND_RISK,
  SIMILAR_BATCH_THRESHOLD,
  type PlanFacts,
} from './approval-risk';

describe('approvalRisk', () => {
  it('maps reversible kinds to low', () => {
    expect(approvalRisk('strategic_plan')).toBe('low');
    expect(approvalRisk('wiki_ingest')).toBe('low');
  });

  it('maps side-effecting kinds to medium', () => {
    expect(approvalRisk('tool_call')).toBe('medium');
    expect(approvalRisk('browser_action')).toBe('medium');
    expect(approvalRisk('skill_activation')).toBe('medium');
  });

  it('maps code-installing / worker-hiring kinds to high', () => {
    expect(approvalRisk('skill_create')).toBe('high');
    expect(approvalRisk('agent_hire')).toBe('high');
  });

  it('defaults unknown kinds to the fail-safe band', () => {
    expect(approvalRisk('some_new_kind')).toBe(UNKNOWN_KIND_RISK);
    expect(UNKNOWN_KIND_RISK).toBe('medium');
  });

  it('escalates when a safety report failed', () => {
    // A low base kind still rises to high on a failed safety report.
    expect(approvalRisk('wiki_ingest', { safety_report: { passed: false } })).toBe('high');
  });

  it('escalates from safety_report.risk_level but never de-escalates', () => {
    expect(approvalRisk('wiki_ingest', { safety_report: { passed: true, risk_level: 'High' } })).toBe('high');
    expect(approvalRisk('wiki_ingest', { safety_report: { passed: true, risk_level: 'Medium' } })).toBe('medium');
    // A high base stays high even with a benign report.
    expect(approvalRisk('agent_hire', { safety_report: { passed: true, risk_level: 'Low' } })).toBe('high');
  });

  it('ignores malformed payloads', () => {
    expect(approvalRisk('tool_call', null)).toBe('medium');
    expect(approvalRisk('tool_call', 'a string')).toBe('medium');
    expect(approvalRisk('tool_call', [1, 2, 3])).toBe('medium');
  });
});

describe('riskTone / riskNeedsConfirm', () => {
  it('maps bands to tokens', () => {
    expect(riskTone('low')).toBe('success');
    expect(riskTone('medium')).toBe('warning');
    expect(riskTone('high')).toBe('danger');
  });

  it('requires confirmation only for high risk', () => {
    expect(riskNeedsConfirm('low')).toBe(false);
    expect(riskNeedsConfirm('medium')).toBe(false);
    expect(riskNeedsConfirm('high')).toBe(true);
  });
});

describe('extractPlanFacts', () => {
  it('pulls tools, targets, and scope from a payload', () => {
    const f = extractPlanFacts({
      tool: 'Bash',
      tools: ['Read', 'Write'],
      url: 'https://example.com',
      path: '/etc/hosts',
      scope: 'workspace-write',
    });
    expect(f.tools).toContain('Bash');
    expect(f.tools).toContain('Read');
    expect(f.tools).toContain('Write');
    expect(f.targets).toContain('https://example.com');
    expect(f.targets).toContain('/etc/hosts');
    expect(f.scope).toBe('workspace-write');
  });

  it('dedupes repeated values', () => {
    const f = extractPlanFacts({ tool: 'Bash', tools: ['Bash', 'Bash'] });
    expect(f.tools).toEqual(['Bash']);
  });

  it('returns empty facts for non-object payloads', () => {
    const empty: PlanFacts = { tools: [], targets: [] };
    expect(extractPlanFacts(null)).toEqual(empty);
    expect(extractPlanFacts('x')).toEqual(empty);
    expect(hasPlanFacts(extractPlanFacts(null))).toBe(false);
  });

  it('reports when facts are present', () => {
    expect(hasPlanFacts(extractPlanFacts({ tool: 'Bash' }))).toBe(true);
    expect(hasPlanFacts(extractPlanFacts({ scope: 'read' }))).toBe(true);
  });
});

describe('fatigue accounting', () => {
  it('counts pending approvals by kind', () => {
    expect(countByKind(['tool_call', 'tool_call', 'browser_action'])).toEqual({
      tool_call: 2,
      browser_action: 1,
    });
  });

  it('surfaces only clusters at/above the threshold, largest first', () => {
    const kinds = ['tool_call', 'tool_call', 'tool_call', 'wiki_ingest', 'browser_action', 'browser_action', 'browser_action', 'browser_action'];
    const batches = similarBatches(kinds);
    expect(batches).toEqual([
      { kind: 'browser_action', count: 4 },
      { kind: 'tool_call', count: 3 },
    ]);
  });

  it('returns no batches below the threshold', () => {
    expect(similarBatches(['tool_call', 'tool_call'])).toEqual([]);
    expect(SIMILAR_BATCH_THRESHOLD).toBe(3);
  });
});

describe('daily counter', () => {
  beforeEach(() => localStorage.clear());

  it('buckets by ISO day', () => {
    expect(isoDay(new Date('2026-07-11T09:30:00Z'))).toBe('2026-07-11');
  });

  it('rolls over across days', () => {
    expect(dailyRollover({ date: '2026-07-11', count: 5 }, '2026-07-11')).toBe(5);
    expect(dailyRollover({ date: '2026-07-10', count: 5 }, '2026-07-11')).toBe(0);
    expect(dailyRollover(null, '2026-07-11')).toBe(0);
  });

  it('increments and persists within a day, resetting on rollover', () => {
    const day1 = new Date('2026-07-11T10:00:00Z');
    expect(readApprovedToday(day1)).toBe(0);
    expect(bumpApprovedToday(day1)).toBe(1);
    expect(bumpApprovedToday(day1)).toBe(2);
    expect(readApprovedToday(day1)).toBe(2);
    const day2 = new Date('2026-07-12T10:00:00Z');
    expect(readApprovedToday(day2)).toBe(0);
    expect(bumpApprovedToday(day2)).toBe(1);
  });
});
