import { mockWsClient } from '@/test/mocks';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import type { ApprovalItem } from '@/lib/api';
import { ApprovalDetailPanel } from './ApprovalDetailPanel';
import { ReachSummary } from '@/components/org/ReachSummary';
import { OrgChangeFacts } from './OrgChangeFacts';
import { useSystemStore } from '@/stores/system-store';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import jaJP from '@/i18n/ja-JP.json';

function orgApproval(): ApprovalItem {
  return {
    id: 'apr-org-1',
    agent_id: 'quote',
    kind: 'org_change',
    summary: 'Move Quote bot to report to Sam',
    created_at: new Date().toISOString(),
    ttl_seconds: 86400,
    payload: {
      agent_id: 'quote',
      before: { reports_to: 'dudu', department: 'Sales' },
      after: { reports_to: 'sales', department: 'Sales' },
      base: 'h1',
      proposer: 'u-lin',
      proposer_email: 'lin@test.invalid',
      reason: 'Sales handles quotes now',
    },
  } as ApprovalItem;
}

function pendingRow(stale: boolean) {
  return {
    approval_id: 'apr-org-1', agent_id: 'quote', before: { reports_to: 'dudu', department: 'Sales' },
    after: { reports_to: 'sales', department: 'Sales' }, base: 'h1', stale, proposer: 'u-lin',
    proposer_email: 'lin@test.invalid', reason: 'Sales handles quotes now', created_at: '2026-11-08T00:00:00Z', expires_at: null,
  };
}

function renderCard(stale: boolean | 'unavailable') {
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method !== 'org.pending_changes') return {};
    if (stale === 'unavailable') throw new Error('nope');
    return { count: 1, pending: [pendingRow(stale)] };
  });
  const onApprove = vi.fn();
  const onReject = vi.fn();
  renderWithProviders(
    <ApprovalDetailPanel approval={orgApproval()} agentName="Quote bot" onApprove={onApprove} onReject={onReject} />,
  );
  return { onApprove, onReject };
}

afterEach(() => {
  mockWsClient.call.mockReset();
  mockWsClient.call.mockResolvedValue(null);
});

describe('org_change approval card', () => {
  it('shows the employee, the before/after placement, who asked and why, with Approve and Reject', async () => {
    const { onApprove, onReject } = renderCard(false);
    const card = await screen.findByTestId('org-change');
    expect(within(card).getByText('Org change request')).toBeInTheDocument();
    expect(within(card).getByText('Quote bot')).toBeInTheDocument();
    expect(within(card).getByTestId('org-change-manager-before')).toHaveTextContent('dudu');
    expect(within(card).getByTestId('org-change-manager-after')).toHaveTextContent('sales');
    expect(within(card).getByText('(unchanged)')).toBeInTheDocument();
    expect(within(card).getByText('lin@test.invalid')).toBeInTheDocument();
    expect(within(card).getByText('Sales handles quotes now')).toBeInTheDocument();
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('org.pending_changes', { agent_id: 'quote' }));
    expect(within(card).queryByRole('alert')).not.toBeInTheDocument();

    const approve = screen.getByRole('button', { name: 'Approve' });
    expect(approve).toBeEnabled();
    fireEvent.click(approve);
    expect(onApprove).toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
    expect(onReject).toHaveBeenCalled();
  });

  it('a stale request says so and can only be rejected', async () => {
    const { onApprove, onReject } = renderCard(true);
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('The organization has changed since this request. Please submit a new one.');
    await waitFor(() => expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled());
    fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
    expect(onApprove).not.toHaveBeenCalled();
    const reject = screen.getByRole('button', { name: 'Reject' });
    expect(reject).toBeEnabled();
    fireEvent.click(reject);
    expect(onReject).toHaveBeenCalled();
  });

  it('still renders from the request itself when the waiting list cannot be read', async () => {
    renderCard('unavailable');
    const card = await screen.findByTestId('org-change');
    expect(within(card).getByTestId('org-change-manager-after')).toHaveTextContent('sales');
    expect(screen.getByRole('button', { name: 'Approve' })).toBeEnabled();
  });
});

it.each([
  { reach: [], text: 'No managers will gain or lose management access.' },
  { reach: null, text: 'The impact cannot be calculated right now.' },
  { reach: [
    { user_id: 'hidden-user', display_name: 'Alice', department: 'Sales', groups: ['schedules', 'skills'], change: 'gains' },
    { user_id: 'hidden-user-2', display_name: 'Bob', department: 'Support', groups: [], change: 'loses' },
  ], text: 'Once approved, Alice (Sales) will be able to manage this AI employee’s schedules and skills; Bob (Support) will no longer be able to manage this AI employee.' },
])('renders reach without internal values: $text', async ({ reach, text }) => {
  mockWsClient.call.mockResolvedValue({ pending: [] });
  const approval = orgApproval();
  approval.payload = { ...(approval.payload as object), reach, reach_digest: 'reach-v1', sentence: 'hidden-server-sentence' };
  const onApprove = vi.fn();
  renderWithProviders(<ApprovalDetailPanel approval={approval} onApprove={onApprove} onReject={vi.fn()} />);
  expect(await screen.findByText(text)).toBeInTheDocument();
  expect(screen.queryByText(/hidden-user|hidden-server-sentence/)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
  expect(onApprove).toHaveBeenCalledWith(undefined, 'reach-v1');
});


it.each([
  { locale: 'zh-TW', messages: zhTW, expected: '核准後，王經理（業務部）將可管理這位 AI 員工的排程、自動化規則、技能、用量與對外傳送審核；李副理（製造部）將不再能管理。' },
  { locale: 'ja-JP', messages: jaJP, expected: '承認後、王經理（業務部）がこの AI 社員のスケジュール、自動化ルール、スキル、使用量と外部送信の審査を管理できるようになります；李副理（製造部）は管理できなくなります。' },
])('builds the localized reach clauses in $locale', ({ locale, messages, expected }) => {
  renderWithProviders(<IntlProvider locale={locale} messages={messages}><ReachSummary reach={[
    { user_id: 'secret-user', display_name: '王經理', department: '業務部', groups: ['schedules', 'automation', 'skills', 'cost', 'egress_review'], change: 'gains' },
    { user_id: 'secret-user-2', display_name: '李副理', department: '製造部', groups: [], change: 'loses' },
  ]} /></IntlProvider>);
  expect(screen.getByText(expected)).toBeInTheDocument();
  expect(screen.queryByText(/secret-user/)).not.toBeInTheDocument();
});

it('uses the matching pending row review when the approval payload has no reach', async () => {
  mockWsClient.call.mockResolvedValue({ pending: [{ ...pendingRow(false), reach: [], reach_digest: 'pending-review' }] });
  const onReachDigest = vi.fn();
  renderWithProviders(<OrgChangeFacts approval={orgApproval()} onReachDigest={onReachDigest} />);
  expect(await screen.findByText('No managers will gain or lose management access.')).toBeInTheDocument();
  await waitFor(() => expect(onReachDigest).toHaveBeenLastCalledWith('pending-review'));
});

it('hides the line and digest in Personal', async () => {
  useSystemStore.setState({ status: { edition_profile: 'personal' } } as never);
  mockWsClient.call.mockResolvedValue({ pending: [] });
  const approval = orgApproval();
  approval.payload = { ...(approval.payload as object), reach: [], reach_digest: 'personal-digest' };
  const onReachDigest = vi.fn();
  renderWithProviders(<OrgChangeFacts approval={approval} onReachDigest={onReachDigest} />);
  expect(screen.queryByTestId('org-reach')).not.toBeInTheDocument();
  await waitFor(() => expect(onReachDigest).toHaveBeenCalledWith(undefined));
  useSystemStore.setState({ status: null });
});
