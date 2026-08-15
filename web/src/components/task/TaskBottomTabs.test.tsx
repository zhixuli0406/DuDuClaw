import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { TaskBottomTabs } from './TaskBottomTabs';
import type { ActivityEvent, TaskComment } from '@/lib/api';

/**
 * I-2a: the four-tab restructure (產物／檔案／變更／過程). Focused on the two
 * behaviors the WP-5F brief calls out specifically —
 *  1. badge counts are known BEFORE a tab is opened (eager fetch), and
 *  2. switching tabs never re-fetches already-loaded data nor unmounts a
 *     panel (keepMounted — the scroll-position-preservation mechanism).
 * Row-level rendering of 產物／變更 content is already covered by
 * `TaskArtifactsPanel.test.tsx` / `TaskChangesPanel.test.tsx` against the
 * same `TaskArtifactsList`/`TaskChangesList` components reused here verbatim.
 */

const EVENTS: ActivityEvent[] = [
  {
    id: 'e1',
    type: 'task_assigned',
    task_id: 't1',
    agent_id: 'nova',
    summary: 'Picked up the task',
    timestamp: '2026-08-15T09:00:00Z',
  },
];
const COMMENTS: TaskComment[] = [
  { id: 'c1', task_id: 't1', body: 'Looks good', author_user: 'u1', created_at: '2026-08-15T09:05:00Z' },
];

function renderTabs(overrides: Partial<Parameters<typeof TaskBottomTabs>[0]> = {}) {
  return renderWithProviders(
    <TaskBottomTabs
      taskId="t1"
      agentId="nova"
      events={EVENTS}
      comments={COMMENTS}
      agents={[{ name: 'nova', display_name: 'Nova' }]}
      onAddComment={vi.fn()}
      {...overrides}
    />,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'tasks.artifacts') {
      return Promise.resolve({
        artifacts: [
          { name: 'a.docx', archived_name: 'a.docx', agent_id: 'nova', origin: 'declared', attribution: 'exact', produced_at: '2026-08-15T09:10:00Z', size: 100, round: 1, channel: null, source_path: null },
          { name: 'b.xlsx', archived_name: 'b.xlsx', agent_id: 'nova', origin: 'swept', attribution: 'exact', produced_at: '2026-08-15T09:11:00Z', size: 200, round: 1, channel: null, source_path: null },
        ],
        truncated: false,
        inferred_count: 0,
      });
    }
    if (method === 'tasks.changes') {
      return Promise.resolve({
        changes: [
          { path: '/repo/a.md', op: 'write', tool_name: 'Write', timestamp: '2026-08-15T09:12:00Z', success: true, snippet: null, source: 'native', round: 1 },
        ],
        distinct_paths: 1,
        truncated: false,
      });
    }
    return Promise.resolve({});
  });
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => ({ files: [] }) }));
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('<TaskBottomTabs> — I-2a tab order and default', () => {
  it('lists the four tabs in 產物／檔案／變更／過程 order, 產物 selected by default', () => {
    renderTabs();
    const tabs = screen.getAllByRole('tab').map((el) => el.textContent);
    expect(tabs[0]).toMatch(/Deliverables/);
    expect(tabs[1]).toMatch(/Files/);
    expect(tabs[2]).toMatch(/Changes/);
    expect(tabs[3]).toMatch(/Process/);
    expect(screen.getByRole('tab', { name: /Deliverables/ })).toHaveAttribute('aria-selected', 'true');
  });
});

describe('<TaskBottomTabs> — eager badge counts (I-2a "分頁帶計數 badge")', () => {
  it('fetches tasks.artifacts and tasks.changes eagerly and badges the tab before either is opened', async () => {
    renderTabs();
    // Badge counts appear without ever clicking a tab.
    expect(await screen.findByText('2')).toBeInTheDocument(); // 產物 badge
    expect(screen.getByText('1')).toBeInTheDocument(); // 變更 badge
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.artifacts', { task_id: 't1' });
    expect(mockWsClient.call).toHaveBeenCalledWith('tasks.changes', { task_id: 't1' });
  });

  it('shows no badge when a count is zero, matching the existing count() convention', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'tasks.artifacts') return Promise.resolve({ artifacts: [], truncated: false, inferred_count: 0 });
      if (method === 'tasks.changes') return Promise.resolve({ changes: [], distinct_paths: 0, truncated: false });
      return Promise.resolve({});
    });
    renderTabs();
    await screen.findByText('This task has no recorded deliverable');
    expect(screen.getByRole('tab', { name: /Deliverables/ }).textContent).toBe('Deliverables');
    expect(screen.getByRole('tab', { name: /Changes/ }).textContent).toBe('Changes');
  });
});

describe('<TaskBottomTabs> — switching tabs never re-fetches (keepMounted)', () => {
  it('fetches 產物/變更 exactly once even after switching away and back several times', async () => {
    const user = userEvent.setup();
    renderTabs();
    await screen.findByText('a.docx'); // artifacts loaded (default tab)

    await user.click(screen.getByRole('tab', { name: /Process/ }));
    await user.click(screen.getByRole('tab', { name: /Changes/ }));
    await screen.findByText('/repo/a.md');
    await user.click(screen.getByRole('tab', { name: /Deliverables/ }));
    await user.click(screen.getByRole('tab', { name: /Changes/ }));

    const artifactCalls = mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.artifacts');
    const changeCalls = mockWsClient.call.mock.calls.filter((c) => c[0] === 'tasks.changes');
    expect(artifactCalls).toHaveLength(1);
    expect(changeCalls).toHaveLength(1);
  });

  it('keeps a switched-away panel mounted in the DOM instead of unmounting it (scroll-position preservation)', async () => {
    const user = userEvent.setup();
    renderTabs();
    await screen.findByText('a.docx'); // 產物 content present while selected

    await user.click(screen.getByRole('tab', { name: /Process/ }));
    // If the panel had unmounted on switch, this would throw. keepMounted
    // keeps it in the DOM (hidden), which is what preserves scroll offset.
    expect(screen.getByText('a.docx')).toBeInTheDocument();
  });

  it('fetches 檔案 lazily — no request until the tab is opened, then never again on repeat visits', async () => {
    const user = userEvent.setup();
    renderTabs();
    await screen.findByText('a.docx');
    expect(fetch).not.toHaveBeenCalled();

    await user.click(screen.getByRole('tab', { name: /Files/ }));
    await screen.findByText('No files for this task');
    expect(fetch).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole('tab', { name: /Deliverables/ }));
    await user.click(screen.getByRole('tab', { name: /Files/ }));
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});

describe('<TaskBottomTabs> — 過程 merges comments + activity (replacing the separate Discussion/Activity tabs)', () => {
  it('renders both a comment and an activity event in one chronological tab', async () => {
    const user = userEvent.setup();
    renderTabs();
    await user.click(screen.getByRole('tab', { name: /Process/ }));
    expect(screen.getByText('Looks good')).toBeInTheDocument();
    expect(screen.getByText('Picked up the task')).toBeInTheDocument();
  });
});
