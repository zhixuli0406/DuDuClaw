import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

// The panel imports decideApproval → api-custom-skills → ws-client. Stub the
// socket so the module graph loads without a live connection.
vi.mock('@/lib/ws-client', () => ({
  client: { call: vi.fn().mockResolvedValue({}) },
}));

import { ApprovalDetailPanel } from './ApprovalDetailPanel';
import type { ApprovalItem } from '@/lib/api';

const SKILL_MD = 'name: my-skill\ndescription: does a thing\n\n# Body\nStep one.';

function skillCreateApproval(payloadOverride?: Record<string, unknown>): ApprovalItem {
  return {
    id: 'apr-1',
    agent_id: 'agent-a',
    kind: 'skill_create',
    summary: '自建技能送審：客服待辦整理',
    created_at: '2026-07-10T00:00:00Z',
    ttl_seconds: 604800,
    payload: {
      custom_skill_id: 'cs-1',
      slug: 'my-skill',
      display_name: '客服待辦整理',
      description_human: '把對話整理成待辦',
      time_saved_value: 30,
      time_saved_unit: 'minutes_per_use',
      tags: 'ops',
      created_by_user: 'user-1',
      built_by_agent: 'agent-a',
      skill_md: SKILL_MD,
      safety_report: {
        passed: true,
        risk_level: 'Low',
        findings: [{ category: 'style', severity: 'low', description: 'prefer explicit tools list', line_number: 3 }],
        sandbox_trial: { ran: false, skip_reason: 'post-install mechanism' },
      },
      ...payloadOverride,
    },
  };
}

describe('<ApprovalDetailPanel> skill_create', () => {
  it('renders the SKILL.md artifact, safety findings, and human fields', () => {
    renderWithProviders(
      <ApprovalDetailPanel approval={skillCreateApproval()} onApprove={vi.fn()} onReject={vi.fn()} />,
    );
    // The artifact that installs on approve is shown verbatim.
    expect(screen.getByText(/name: my-skill/)).toBeInTheDocument();
    // Safety finding surfaces.
    expect(screen.getByText(/prefer explicit tools list/)).toBeInTheDocument();
    // Risk level badge.
    expect(screen.getByText('Low')).toBeInTheDocument();
    // Human display name.
    expect(screen.getByText('客服待辦整理')).toBeInTheDocument();
  });

  it('falls back to the generic raw-payload view when the SKILL.md is absent', () => {
    const approval = skillCreateApproval({ skill_md: '' });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={vi.fn()} onReject={vi.fn()} />);
    // The summary is shown by the generic branch; no SKILL.md pre block.
    expect(screen.getByText(approval.summary)).toBeInTheDocument();
    expect(screen.queryByText(/name: my-skill/)).not.toBeInTheDocument();
  });
});
