import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

// The panel imports decideApproval → api-custom-skills → ws-client. Stub the
// socket so the module graph loads without a live connection.
vi.mock('@/lib/ws-client', () => ({
  client: {
    call: vi.fn().mockResolvedValue({}),
    // agents-store (pulled in via CharacterAvatar → wardrobe outfit lookup)
    // subscribes at module init; a no-op keeps the graph loadable offline.
    subscribe: vi.fn(),
  },
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

  it('falls back to the generic view when the SKILL.md is absent', () => {
    const approval = skillCreateApproval({ skill_md: '' });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={vi.fn()} onReject={vi.fn()} />);
    // The summary is shown by the generic branch; no SKILL.md pre block.
    expect(screen.getByText(approval.summary)).toBeInTheDocument();
    expect(screen.queryByText(/name: my-skill/)).not.toBeInTheDocument();
  });
});

function genericApproval(over?: Partial<ApprovalItem>): ApprovalItem {
  return {
    id: 'apr-2',
    agent_id: 'agent-b',
    kind: 'tool_call',
    summary: 'Run a shell command to tidy the logs',
    created_at: '2026-07-11T00:00:00Z',
    ttl_seconds: 3600,
    payload: { tool: 'Bash', command: 'rm /tmp/old.log', scope: 'workspace-write' },
    ...over,
  };
}

describe('<ApprovalDetailPanel> generic (U2 redesign)', () => {
  it('leads with the plan summary + a whole-action risk badge', () => {
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval()} onApprove={vi.fn()} onReject={vi.fn()} />);
    // Plan-first: "What this AI employee plans to do" heading + plain-language kind.
    expect(screen.getByText('What this AI employee plans to do')).toBeInTheDocument();
    expect(screen.getByText('Call a tool to carry out an action')).toBeInTheDocument();
    // Whole-action risk badge (tool_call → medium).
    expect(screen.getByText('Medium risk')).toBeInTheDocument();
    // Scope-of-impact facts surface as verification aids.
    expect(screen.getByText('Bash')).toBeInTheDocument();
    expect(screen.getByText('workspace-write')).toBeInTheDocument();
  });

  it('keeps the raw payload behind an opt-in spot-check', () => {
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval()} onApprove={vi.fn()} onReject={vi.fn()} />);
    // The full JSON payload is not force-read.
    expect(screen.queryByText(/"command": "rm \/tmp\/old.log"/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByText('Spot-check full details'));
    expect(screen.getByText(/"command": "rm \/tmp\/old.log"/)).toBeInTheDocument();
  });

  it('approves medium-risk actions directly', () => {
    const onApprove = vi.fn();
    renderWithProviders(<ApprovalDetailPanel approval={genericApproval()} onApprove={onApprove} onReject={vi.fn()} />);
    fireEvent.click(screen.getByText('Approve'));
    expect(onApprove).toHaveBeenCalledTimes(1);
  });

  it('gates high-risk approval behind a second confirmation', () => {
    const onApprove = vi.fn();
    const approval = genericApproval({ kind: 'agent_hire', summary: 'Hire a data-entry AI employee' });
    renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={onApprove} onReject={vi.fn()} />);
    expect(screen.getByText('High risk')).toBeInTheDocument();
    // First click opens the confirm dialog — it does NOT approve yet.
    fireEvent.click(screen.getByText('Approve'));
    expect(onApprove).not.toHaveBeenCalled();
    // Confirming in the dialog runs the approval exactly once.
    fireEvent.click(screen.getByText('Confirm approve'));
    expect(onApprove).toHaveBeenCalledTimes(1);
  });
});
