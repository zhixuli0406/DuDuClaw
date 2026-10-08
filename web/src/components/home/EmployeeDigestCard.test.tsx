import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { EmployeeDigestCard } from './EmployeeDigestCard';

function renderCard() {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter>
        <EmployeeDigestCard enabled />
      </MemoryRouter>
    </IntlProvider>,
  );
}

const digest = {
  date: '2026-10-08',
  since: '2026-10-07T00:00:00Z',
  generated_at: '2026-10-08T00:00:00Z',
  agents: [
    {
      agent_id: 'alice',
      finished: [{ kind: 'task', id: 't1', title: 'Weekly report', completed_at: null }],
      finished_total: 1,
      activity_count: 3,
      pending_approvals: 0,
      runs_done: 1,
      runs_not_done: 0,
      spend_usd: 0.12,
    },
  ],
};

beforeEach(() => vi.clearAllMocks());

describe('EmployeeDigestCard (P9)', () => {
  it('renders nothing when the feature is off', async () => {
    mockWsClient.call.mockResolvedValue({ enabled: false, digest: digest, feedback: {} });
    const { container } = renderCard();
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalled());
    expect(container.querySelector('[data-testid="employee-digest"]')).toBeNull();
  });

  it('lists finished work and sends a thumbs-up for that task', async () => {
    const calls: Array<[string, unknown]> = [];
    mockWsClient.call.mockImplementation((m: string, p?: unknown) => {
      calls.push([m, p]);
      if (m === 'digest.latest') return Promise.resolve({ enabled: true, digest, feedback: {} });
      return Promise.resolve({ task_id: 't1', verdict: 'up' });
    });
    renderCard();
    expect(await screen.findByText('Weekly report')).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole('button', { name: 'Good result' }));
    await waitFor(() =>
      expect(calls.find(([m]) => m === 'digest.feedback')?.[1]).toEqual({ task_id: 't1', verdict: 'up' }),
    );
    expect(await screen.findByText('👍 rated')).toBeInTheDocument();
  });
});
