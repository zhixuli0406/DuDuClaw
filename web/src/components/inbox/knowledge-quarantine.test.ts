import { describe, it, expect } from 'vitest';
import {
  approvalListTitle,
  parseKnowledgeQuarantine,
  quarantineResultMessage,
} from './knowledge-quarantine';

// Shapes copied from crates/duduclaw-gateway/src/wiki_ingest.rs
// `dispatch_quarantine_side_effects` and handlers/approvals_decide.rs.
const CONFLICT_SUMMARY =
  '對話中出現一則與現有記憶「王小明」衝突的說法，但它的來源可信度較低（trust: conversation_distill 0.60 < operator 1.00），' +
  '所以沒有取代現有內容。已暫存 1 筆；核准後會以您的權限取代現有內容，拒絕則捨棄。內容摘要：王小明的生日是 3 月 5 日';
const CONFLICT_PAYLOAD = {
  memory_db: '/home/x/memory.db',
  agent_id: 'assistant',
  origin: 'conversation_distill',
  subject: '王小明',
  quarantined_ids: ['m1'],
  promote_on_approve: true,
};
const BURST_PAYLOAD = { ...CONFLICT_PAYLOAD, promote_on_approve: undefined, quarantined_ids: ['a', 'b', 'c'] };

const fmt = (d: { id: string }, v?: Record<string, string | number>) =>
  `${d.id}${v ? JSON.stringify(v) : ''}`;

describe('parseKnowledgeQuarantine', () => {
  it('reads a trust-held conflict: subject from payload, statement from the summary', () => {
    expect(parseKnowledgeQuarantine('knowledge_quarantine', CONFLICT_PAYLOAD, CONFLICT_SUMMARY)).toEqual({
      variant: 'conflict',
      subject: '王小明',
      statement: '王小明的生日是 3 月 5 日',
      count: 1,
    });
  });

  it('treats a payload without promote_on_approve as the burst case', () => {
    const v = parseKnowledgeQuarantine('knowledge_quarantine', BURST_PAYLOAD, '偵測到…內容摘要：x');
    expect(v?.variant).toBe('burst');
    expect(v?.count).toBe(3);
  });

  it('also accepts an explicit trust_held disposition', () => {
    expect(
      parseKnowledgeQuarantine('knowledge_quarantine', { disposition: 'trust_held' }, 's')?.variant,
    ).toBe('conflict');
  });

  it('has no statement when the summary carries no snippet, and ignores other kinds', () => {
    expect(parseKnowledgeQuarantine('knowledge_quarantine', CONFLICT_PAYLOAD, 'plain')?.statement).toBeUndefined();
    expect(parseKnowledgeQuarantine('tool_call', CONFLICT_PAYLOAD, CONFLICT_SUMMARY)).toBeNull();
  });
});

describe('quarantineResultMessage', () => {
  const id = (se: Record<string, unknown> | null, v?: 'conflict' | 'burst') =>
    quarantineResultMessage({ side_effect: se }, v);

  it('maps each server side effect to its message', () => {
    expect(id({ quarantine_promoted: 1 }, 'conflict')).toEqual({ id: 'approval.knowledge.result.promoted' });
    expect(id({ quarantine_released: 3 }, 'burst')).toEqual({ id: 'approval.knowledge.result.released' });
    expect(id({ quarantine_rejected: 1 }, 'conflict')).toEqual({
      id: 'approval.knowledge.result.conflictRejected',
    });
    expect(id({ quarantine_rejected: 3 }, 'burst')).toEqual({ id: 'approval.knowledge.result.rejected' });
  });

  it('reports a stale decision (nothing written)', () => {
    expect(id({ quarantine_stale: 1 }, 'conflict')).toEqual({ id: 'approval.knowledge.result.stale' });
  });

  it('reports items held back from a burst, alone or with released ones', () => {
    expect(id({ quarantine_released: 2, quarantine_held: 1 }, 'burst')).toEqual({
      id: 'approval.knowledge.result.releasedAndHeld',
      values: { released: 2, held: 1 },
    });
    expect(id({ quarantine_released: 0, quarantine_held: 3 }, 'burst')).toEqual({
      id: 'approval.knowledge.result.allHeld',
      values: { held: 3 },
    });
    expect(id({ quarantine_held: 2 }, 'burst')).toEqual({
      id: 'approval.knowledge.result.allHeld',
      values: { held: 2 },
    });
    // held: 0 reads as a plain release
    expect(id({ quarantine_released: 2, quarantine_held: 0 }, 'burst')).toEqual({
      id: 'approval.knowledge.result.released',
    });
  });

  it('falls back (null) when there is no quarantine side effect', () => {
    expect(id(null)).toBeNull();
    expect(id({ installed_skill: 'x' })).toBeNull();
    expect(quarantineResultMessage(undefined)).toBeNull();
  });
});

describe('parseKnowledgeQuarantine with the new payload fields', () => {
  const summary =
    '對話中有一則關於「偏好語言」的新說法，和系統目前採用、來源更可靠的內容不一致，所以還沒有套用。' +
    '目前內容：「中文」。核准會改用這則新說法取代目前內容；拒絕則捨棄這則新說法。內容摘要：prefers_language: 英文';
  const payload = {
    subject: 'user:42',
    quarantined_ids: ['h1'],
    promote_on_approve: true,
    disposition: 'trust_held',
    predicate: 'prefers_language',
    snippet: 'prefers_language: 英文',
    existing_id: 'e1',
    existing_content: '中文',
    reason: 'trust: profile_distill 0.60 < operator 1.00',
  };

  it('uses snippet / existing_content and the plain topic label from the summary', () => {
    expect(parseKnowledgeQuarantine('knowledge_quarantine', payload, summary)).toEqual({
      variant: 'conflict',
      subject: '偏好語言',
      statement: 'prefers_language: 英文',
      current: '中文',
      count: 1,
    });
  });

  it('never exposes the machine reason', () => {
    const v = parseKnowledgeQuarantine('knowledge_quarantine', payload, summary);
    expect(JSON.stringify(v)).not.toContain('trust:');
  });

  it('falls back to payload subject and summary cut for older rows', () => {
    const v = parseKnowledgeQuarantine(
      'knowledge_quarantine',
      { subject: '王小明', promote_on_approve: true, quarantined_ids: ['m1'] },
      '舊格式…內容摘要：生日 3/5',
    );
    expect(v).toMatchObject({ subject: '王小明', statement: '生日 3/5', current: undefined });
  });
});

describe('approvalListTitle', () => {
  it('replaces the server sentence for a conflict and keeps every other summary', () => {
    const conflict = { kind: 'knowledge_quarantine', payload: CONFLICT_PAYLOAD, summary: CONFLICT_SUMMARY };
    expect(approvalListTitle(conflict, fmt)).toBe('approval.knowledge.rowTitle{"subject":"王小明"}');
    expect(approvalListTitle({ ...conflict, payload: { promote_on_approve: true } }, fmt)).toBe(
      'approval.knowledge.rowTitleNoSubject',
    );
    expect(approvalListTitle({ ...conflict, payload: BURST_PAYLOAD }, fmt)).toBe(CONFLICT_SUMMARY);
    expect(approvalListTitle({ kind: 'tool_call', payload: {}, summary: 'S' }, fmt)).toBe('S');
  });
});

describe('parseKnowledgeQuarantine — full-write payload contract', () => {
  const longStatement = '使用者希望被稱為「小明」。' + '補充說明'.repeat(200) + '\n第二行';
  const payload = {
    disposition: 'trust_held',
    promote_on_approve: true,
    quarantined_ids: ['h1'],
    subject: 'user:42',
    subject_label: '王小明的個人資料：希望的稱呼',
    predicate: 'preferred_name',
    snippet: longStatement,
    new_value: '小明',
    existing_content: '使用者希望被稱為「王先生」。',
    existing_content_truncated: true,
    existing_value: '王先生',
    existing_id: 'e1',
    claim_digest: 'sha256:abc',
    origin_channel: 'telegram',
    origin_chat_id: '12345',
    reason: 'trust: x 0.6 < y 1.0',
  };

  it('reads every display field and keeps the statement verbatim', () => {
    const v = parseKnowledgeQuarantine('knowledge_quarantine', payload, '…關於「舊的」的新說法…內容摘要：cut');
    expect(v).toMatchObject({
      variant: 'conflict',
      subject: '王小明的個人資料：希望的稱呼',
      statement: longStatement,
      newValue: '小明',
      current: '使用者希望被稱為「王先生」。',
      currentTruncated: true,
      currentValue: '王先生',
    });
  });

  it('never carries internal fields', () => {
    const s = JSON.stringify(parseKnowledgeQuarantine('knowledge_quarantine', payload, ''));
    for (const hidden of ['preferred_name', 'user:42', 'sha256:abc', '12345', 'trust:', 'e1"']) {
      expect(s).not.toContain(hidden);
    }
  });

  it('renders non-string values as text and tolerates their absence', () => {
    const v = parseKnowledgeQuarantine(
      'knowledge_quarantine',
      { promote_on_approve: true, new_value: 30, existing_value: false },
      '',
    );
    expect(v).toMatchObject({ newValue: '30', currentValue: 'false', currentTruncated: undefined });
  });
});

