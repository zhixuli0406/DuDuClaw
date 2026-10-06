import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { WorkflowActivationFacts } from './WorkflowActivationFacts';
import type { ApprovalItem } from '@/lib/api';

const base: ApprovalItem = {
  id: 'a1', agent_id: 'alice', kind: 'workflow_activation', summary: 's',
  payload: { redacted: true }, created_at: new Date().toISOString(), ttl_seconds: 3600,
};

describe('WorkflowActivationFacts', () => {
  it('shows each effect target, the admin-only rule and self-approval', () => {
    renderWithProviders(<WorkflowActivationFacts approval={{
      ...base, may_decide: true, submitter_is_viewer: true,
      workflow_activation: { effect_targets: [{ step_id: 'update', tool: 'tasks_update', param: 'task_id', target: 'task-42' }] },
    }} />);
    expect(screen.getByText('task-42')).toBeInTheDocument();
    expect(screen.getByText(/Only an Admin/)).toBeInTheDocument();
    expect(screen.getByRole('note')).toHaveTextContent(/submitter and the approver are the same person/);
  });

  it('tells a viewer who may not decide, without targets', () => {
    renderWithProviders(<WorkflowActivationFacts approval={{ ...base, may_decide: false, workflow_activation: null }} />);
    expect(screen.getByText(/cannot decide it/)).toBeInTheDocument();
    expect(screen.queryByRole('note')).toBeNull();
  });

  it('shows how long the approval lasts and that only count limits apply without prices', () => {
    renderWithProviders(<WorkflowActivationFacts approval={{
      ...base, may_decide: true,
      workflow_activation: { effect_targets: [], expires_at: '2026-11-04T00:00:00Z', pricing: { money_limits_effective: false } },
    }} />);
    expect(screen.getByText(/stays active until 2026-11-04T00:00:00Z/)).toBeInTheDocument();
    expect(screen.getByRole('note')).toHaveTextContent(/only the count limits/);
  });
});
