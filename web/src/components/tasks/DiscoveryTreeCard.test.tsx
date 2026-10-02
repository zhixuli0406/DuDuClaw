import { describe, expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { DiscoveryTreeCard } from './DiscoveryTreeCard';
import type { DiscoveryTree } from '@/lib/discovery-api';

const cost = { usd: 0, usd_source: 'unknown' as const, unknown_calls: 1, wall_secs: 2, input_tokens: 0, output_tokens: 0, cache_read_tokens: 0 };
export const fixture: DiscoveryTree = {
  run: { run_id: 'run-1', task_id: 'task-1', title: 'Explore actual alternatives', status: 'running', approval_status: 'approved', current_round: 1, branch_count: 2, refine_count: 1, runtime: 'codex', budget: { max_agent_calls: 4, max_usd: 1, max_wall_secs: 90, max_rounds: 2 }, cost, can_cancel: true, artifact_verified: false, degraded: true, isolation_degraded: true },
  nodes: [ { cell_id: 'r1-b0-a0', round: 1, branch: 0, attempt: 0, parent_id: null, status: 'ok', score: 2.5, model: null, cost }, { cell_id: 'r1-b1-a0', round: 1, branch: 1, attempt: 0, parent_id: 'r1-b0-a0', status: 'running', score: null, model: null, cost } ],
  rounds: [{ round: 1, policy_id: 'uniform', beta: 0, full_grid: ['r1-b0-a0', 'r1-b1-a0'], completion: ['r1-b0-a0'], new_in: ['r1-b1-a0'] }],
};

describe('DiscoveryTreeCard', () => {
  it('summarizes each round from its recorded grid when the run plan changes', () => {
    const tree: DiscoveryTree = {
      ...fixture,
      run: { ...fixture.run, branch_count: 8, refine_count: 3 },
      rounds: [
        { ...fixture.rounds[0], full_grid: ['r1-b0-a0', 'r1-b0-a1', 'r1-b1-a0', 'r1-b1-a1'] },
        { ...fixture.rounds[0], round: 2, full_grid: ['r2-b0-a0', 'r2-b1-a0', 'r2-b2-a0'], completion: [] },
      ],
    };
    renderWithProviders(<DiscoveryTreeCard tree={tree} onCancel={vi.fn()} onDownload={vi.fn()} />);
    expect(screen.getByText('Round 1 · Planned: 4 · 1/4 completed', { selector: 'summary' })).toBeInTheDocument();
    expect(screen.getByText('Round 2 · Planned: 3 · 0/3 completed', { selector: 'summary' })).toBeInTheDocument();
  });

  it('shows explicit parent edges, the full grid and unknown costs without pretending zero is a bill', () => {
    renderWithProviders(<DiscoveryTreeCard tree={fixture} onCancel={vi.fn()} onDownload={vi.fn()} />);
    expect(screen.getByText('Explore actual alternatives')).toBeInTheDocument();
    expect(screen.getAllByText(/Unknown cost/).length).toBeGreaterThan(0);
    expect(screen.getByText(/r1-b0-a0 → r1-b1-a0/)).toBeInTheDocument();
    expect(screen.getByText(/Grid: r1-b0-a0, r1-b1-a0/)).toBeInTheDocument();
    expect(screen.getByText(/Completed: r1-b0-a0/)).toBeInTheDocument();
    expect(screen.getByText(/operating-system isolation/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Download/ })).not.toBeInTheDocument();
  });
  it('uses the backend capability for cancellation and disables a duplicate request', async () => {
    let finish!: () => void;
    const cancel = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    const user = userEvent.setup();
    renderWithProviders(<DiscoveryTreeCard tree={fixture} onCancel={cancel} onDownload={vi.fn()} />);
    await user.click(screen.getByRole('button', { name: 'Cancel exploration' }));
    expect(cancel).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'Cancel exploration' })).toBeDisabled();
    finish();
  });
  it('does not grant cancel permission or artifact delivery from a completed node alone', () => {
    renderWithProviders(<DiscoveryTreeCard tree={{ ...fixture, run: { ...fixture.run, can_cancel: false } }} onCancel={vi.fn()} onDownload={vi.fn()} />);
    expect(screen.queryByRole('button', { name: 'Cancel exploration' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Download/ })).not.toBeInTheDocument();
  });
});

it('distinguishes a reported zero bill from an estimate',()=>{
  const tree={...fixture,run:{...fixture.run,cost:{...fixture.run.cost,usd:0,usd_source:'reported' as const}},nodes:[{...fixture.nodes[0],cost:{...cost,usd:0.00125,usd_source:'estimated' as const}}]};
  renderWithProviders(<DiscoveryTreeCard tree={tree} onCancel={vi.fn()} onDownload={vi.fn()}/>);
  expect(screen.getByText('Actual: $0.00')).toBeInTheDocument();
  expect(screen.getByText('Estimated: $0.00125')).toBeInTheDocument();
});

it('shows the full planned grid even for a cell that has no ledger node yet',()=>{
  const tree={...fixture,rounds:[{...fixture.rounds[0],full_grid:[...fixture.rounds[0].full_grid,'r1-b0-a1']}]};
  renderWithProviders(<DiscoveryTreeCard tree={tree} onCancel={vi.fn()} onDownload={vi.fn()}/>);
  expect(screen.getByText(/Grid:.*r1-b0-a1/)).toBeInTheDocument();
});

const withRun=(patch:Record<string,unknown>)=>({...fixture,run:{...fixture.run,...patch}}) as DiscoveryTree;
const show=(patch:Record<string,unknown>)=>renderWithProviders(<DiscoveryTreeCard tree={withRun(patch)} onCancel={vi.fn()} onDownload={vi.fn()}/>);

describe('run explanations',()=>{
  it('shows the isolation banner only when isolation was really lost',()=>{
    show({degraded:true,isolation_degraded:false});
    expect(screen.queryByText(/isolation/i)).not.toBeInTheDocument();
  });
  it('explains a stop code in plain language and falls back for unknown codes',()=>{
    const {unmount}=show({status:'degraded',degraded:true,isolation_degraded:false,stop_code:'no_account'});
    expect(screen.getByText(/No usable AI account/)).toBeInTheDocument();
    unmount();
    show({stop_code:'brand_new_code'});
    expect(screen.getByText(/ended early for an internal reason/)).toBeInTheDocument();
  });
  it('explains a tool_violation stop and shows the runtime product name',()=>{
    show({runtime:'openai-compat',stop_code:'tool_violation'});
    expect(screen.getByText(/used a tool that is not allowed/)).toBeInTheDocument();
    expect(screen.getByText(/OpenAI-compatible/)).toBeInTheDocument();
    expect(screen.queryByText(/ended early for an internal reason/)).not.toBeInTheDocument();
  });
  it('localizes run, approval and node statuses with raw fallback',()=>{
    show({status:'pending_approval',approval_status:'denied'});
    expect(screen.getByText(/Awaiting approval/)).toBeInTheDocument();
    expect(screen.getByText(/Rejected by manager/)).toBeInTheDocument();
    expect(screen.queryByText(/pending_approval/)).not.toBeInTheDocument();
  });
  it('falls back to the raw value for unknown statuses',()=>{
    show({status:'weird_state',approval_status:'decided'});
    expect(screen.getByText(/weird_state/)).toBeInTheDocument();
    expect(screen.getByText(/Decided/)).toBeInTheDocument();
  });
  it('renders node statuses from the node vocabulary',()=>{
    renderWithProviders(<DiscoveryTreeCard tree={{...fixture,nodes:[{...fixture.nodes[0],status:'invalid_solution'},{...fixture.nodes[1],status:'valid'}]}} onCancel={vi.fn()} onDownload={vi.fn()}/>);
    expect(screen.getByText('Invalid solution')).toBeInTheDocument();
    expect(screen.getByText('Valid')).toBeInTheDocument();
  });
  it('says why a run was cancelled',()=>{
    show({status:'cancelled',approval_status:'denied',cancel_code:'approval_denied'});
    expect(screen.getByText(/manager rejected this exploration request/)).toBeInTheDocument();
  });
  it('tells the requester where approval happens and when it expires',()=>{
    show({status:'pending_approval',approval_status:'pending',approval_expires_at:'2026-10-02T08:00:00Z'});
    expect(screen.getByText(/approve it in the Inbox/)).toBeInTheDocument();
    expect(screen.getByText(/cancelled automatically/)).toBeInTheDocument();
  });
  it('shows the approval hint without a time for an older server',()=>{
    show({status:'pending_approval',approval_status:'pending',approval_expires_at:null});
    expect(screen.getByText(/approve it in the Inbox/)).toBeInTheDocument();
  });
  it.each([[0.946662833,'0.9s'],[0,'0s'],[125.04,'125s']])('formats elapsed %s',(secs,text)=>{
    show({cost:{...cost,wall_secs:secs}});
    expect(screen.getByText(new RegExp(`${text} elapsed`))).toBeInTheDocument();
  });
});
