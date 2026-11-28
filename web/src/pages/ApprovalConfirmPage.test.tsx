import { StrictMode } from 'react';
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route, useLocation } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { ApprovalConfirmPage } from './ApprovalConfirmPage';
import { approvalConfirmReturn } from '@/lib/approval-step-up';
import { useAuthStore } from '@/stores/auth-store';
import { dashboardEndpoint } from '@/lib/dashboard-base';

const ticket = 'ab'.repeat(32);
const token = Array.from({ length: 32 }, (_, i) => i);
const preview = {
  status: 'live', kind_label: 'External send', digest: 'digest-reviewed', has_totp: true,
  expires: Math.floor(Date.now() / 1000) + 600, agent_file_change: null,
  approval: { id: 'approval-private', kind: 'external_send', summary: 'Send the quote', payload: { content: '<script>full email</script>' } },
};
const fetchMock = vi.fn();
function response(value: unknown, status = 200) { return { ok: status === 200, status, json: async () => value }; }
function page(hash = `#ticket=${ticket}`) {
  window.history.replaceState(null, '', `/approvals/confirm${hash}`);
  return render(<IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[`/approvals/confirm${hash}`]}>
    <Routes><Route path="/approvals/confirm" element={<ApprovalConfirmPage />} /><Route path="/login" element={<LoginProbe />} /></Routes>
  </MemoryRouter></IntlProvider>);
}
function LoginProbe() { return <div data-testid="login-from">{(useLocation().state as { from: string })?.from}</div>; }
async function approve() {
  await screen.findByText('Send the quote');
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'secret' } });
  fireEvent.change(screen.getByLabelText('6-digit authenticator app code'), { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
}
beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal('fetch', fetchMock);
  fetchMock.mockReset().mockResolvedValue(response(preview));
  useAuthStore.setState({ jwt: 'session-jwt', logout: vi.fn().mockResolvedValue(undefined) });
  mockWsClient.call.mockImplementation((method: string) => method === 'isolation.connector_step_up'
    ? Promise.resolve({ verified: true, step_up_csrf: token })
    : Promise.resolve({ approvals: [{ id: preview.approval.id, decided_by_display: 'Alice' }] }));
});
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); });

it('reads an exact fragment and immediately removes it before GET', async () => {
  const replace = vi.spyOn(window.history, 'replaceState');
  page();
  expect(window.location.hash).toBe('');
  expect(replace).toHaveBeenLastCalledWith(null, '', '/approvals/confirm');
  await screen.findByText('Send the quote');
  const [url, options] = fetchMock.mock.calls[0];
  expect(url).toBe(dashboardEndpoint('/api/me/approvals/step-up'));
  expect(options).toMatchObject({ method: 'GET', headers: { Authorization: 'Bearer session-jwt', 'X-Approval-Ticket': ticket }, cache: 'no-store', referrerPolicy: 'no-referrer', credentials: 'omit' });
  expect(url).not.toContain(ticket); expect(options.body).toBeUndefined();
  expect(screen.getByText(/<script>full email<\/script>/)).toBeInTheDocument();
  expect(document.querySelector('script')).toBeNull();
});
it.each(['', '#ticket=bad', `#ticket=${'A'.repeat(64)}`, `#ticket=${ticket}&extra=1`])('rejects missing or malformed fragment %s', async hash => {
  page(hash);
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.notFound']);
  expect(fetchMock).not.toHaveBeenCalled();
  expect(window.location.hash).toBe('');
});
it('only allows exact relative confirmation return targets', () => {
  expect(approvalConfirmReturn(`/approvals/confirm#ticket=${ticket}`)).toBe(`/approvals/confirm#ticket=${ticket}`);
  for (const value of ['/inbox', 'https://evil.test', `/approvals/confirm?x=1#ticket=${ticket}`, `/approvals/confirm#ticket=${ticket}\n`, '/approvals/confirm#ticket=bad', null]) expect(approvalConfirmReturn(value)).toBeUndefined();
});
it('verifies identity, posts the reviewed digest and proof once, clears fields and shows only display attribution', async () => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response({ status: 'done', decision: 'approve', decided_by: 'INTERNAL-SECRET-ID' }));
  page(); await approve();
  expect(await screen.findByText(/✓ Approved: External send/)).toHaveTextContent('(Alice)');
  expect(mockWsClient.call).toHaveBeenCalledWith('isolation.connector_step_up', { password: 'secret', code: '123456' });
  expect(mockWsClient.call).toHaveBeenCalledWith('approvals.list', { id: preview.approval.id });
  const [url, options] = fetchMock.mock.calls[1];
  expect(options).toMatchObject({ method: 'POST', headers: { 'X-Approval-Ticket': ticket }, cache: 'no-store', referrerPolicy: 'no-referrer', credentials: 'omit' });
  expect(JSON.parse(options.body)).toEqual({ decision: 'approve', expected_digest: preview.digest, step_up_csrf: token });
  expect(url).not.toContain(ticket); expect(options.body).not.toContain(ticket);
  expect(document.body.textContent).not.toContain('INTERNAL-SECRET-ID');
  expect(fetchMock).toHaveBeenCalledTimes(2);
});
it('rejects without credentials or step-up proof', async () => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response({ status: 'done', decision: 'reject', decided_by: 'INTERNAL-SECRET-ID' }));
  page(); await screen.findByText('Send the quote');
  fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
  expect(await screen.findByText(/✗ Rejected: External send/)).toBeInTheDocument();
  expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ decision: 'reject', expected_digest: preview.digest });
  expect(mockWsClient.call.mock.calls.some(([method]) => method === 'isolation.connector_step_up')).toBe(false);
  expect(document.body.textContent).not.toContain('INTERNAL-SECRET-ID');
});
it('does not retry a spent POST after a refusal', async () => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response({ status: 'forbidden', error: 'Server refusal.' }, 403));
  page(); await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent('Server refusal.');
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledTimes(2);
});
it('does not retry an uncertain POST after a network failure', async () => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockRejectedValueOnce(new Error('network'));
  page(); await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(fetchMock).toHaveBeenCalledTimes(2);
  expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
});
it.each([
  [401, 'unauthenticated', 'loginAgain'], [404, 'not_found', 'notFound'],
  [410, 'expired', 'expired'], [409, 'used', 'expired'], [409, 'digest_mismatch', 'changed'],
  [409, 'already_decided', 'alreadyDecided'], [403, 'forbidden', null],
  [403, 'feature_off', null], [403, 'not_supported', null], [403, 'step_up_required', 'verifyAgain'],
  [400, 'invalid_request', null], [503, 'unavailable', null], [503, 'internal', null],
] as const)('maps HTTP %s %s to the correct state', async (http, status, key) => {
  fetchMock.mockResolvedValueOnce(response({ status, error: 'Server sentence.' }, http));
  page();
  expect(await screen.findByRole('alert')).toHaveTextContent(key ? en[`approvalConfirm.${key}` as keyof typeof en] : 'Server sentence.');
  expect(screen.getByRole('link', { name: 'Go to inbox' })).toHaveAttribute('href', '/inbox');
  if (status === 'not_found') {
    fireEvent.click(screen.getByRole('button', { name: 'Sign out' }));
    expect(useAuthStore.getState().logout).toHaveBeenCalledOnce();
  }
  if (status === 'unauthenticated') {
    fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(await screen.findByTestId('login-from')).toHaveTextContent(`/approvals/confirm#ticket=${ticket}`);
  }
});
it('requires a fresh manual verification after step_up_required, without retaining credentials', async () => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response({ status: 'step_up_required', error: 'Again' }, 403));
  page(); await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.verifyAgain']);
  expect(screen.getByLabelText('Password')).toHaveValue('');
  expect(screen.getByLabelText('6-digit authenticator app code')).toHaveValue('');
  expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled();
  expect(fetchMock).toHaveBeenCalledTimes(2);
});
it('clears failed verification fields and never posts', async () => {
  mockWsClient.call.mockRejectedValueOnce(new Error('bad credentials'));
  page(); await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.verifyAgain']);
  expect(screen.getByLabelText('Password')).toHaveValue('');
  expect(screen.getByLabelText('6-digit authenticator app code')).toHaveValue('');
  expect(fetchMock).toHaveBeenCalledOnce();
});
it('offers only rejection and inbox when no authenticator is configured', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, has_totp: false }));
  page(); await screen.findByText(en['approvalConfirm.noTotp']);
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  expect(screen.queryByLabelText('Password')).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Reject' })).toBeEnabled();
});
it('expires locally and disables decisions', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, expires: Math.floor(Date.now() / 1000) - 1 }));
  page(); expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.expired']);
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
});
it('still shows success if the attribution follow-up fails', async () => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response({ status: 'done', decision: 'reject', decided_by: 'HIDDEN' }));
  mockWsClient.call.mockRejectedValueOnce(new Error('offline'));
  page(); await screen.findByText('Send the quote'); fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
  expect(await screen.findByText('✗ Rejected: External send')).toBeInTheDocument();
  await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
  expect(document.body.textContent).not.toContain('HIDDEN');
});

it('does not duplicate GET when effects replay in StrictMode', async () => {
  render(<StrictMode><IntlProvider locale="en" messages={en}><MemoryRouter initialEntries={[`/approvals/confirm#ticket=${ticket}`]}>
    <ApprovalConfirmPage />
  </MemoryRouter></IntlProvider></StrictMode>);
  await screen.findByText('Send the quote');
  expect(fetchMock).toHaveBeenCalledOnce();
});
it('locks both decisions while verification is pending', async () => {
  let finish!: (value: unknown) => void;
  mockWsClient.call.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response({ status: 'done', decision: 'approve', decided_by: 'HIDDEN' }));
  page(); await approve();
  expect(screen.getByRole('button', { name: 'Reject' })).toBeDisabled();
  fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
  fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
  finish({ verified: true, step_up_csrf: token });
  expect(await screen.findByText(/✓ Approved:/)).toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledTimes(2);
});
it('uses the file-change HTTP projection without fetching a second version', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview,
    approval: { ...preview.approval, kind: 'agent_file_change', payload: {} },
    agent_file_change: { approval_id: preview.approval.id, agent_id: 'Employee', file: 'soul',
      requested_by: 'Owner', reason: 'File review reason', verdict: 'pass', reason_categories: [],
      stale: false, content_available: false, diff: null },
  }));
  page();
  expect(await screen.findByText('File review reason')).toBeInTheDocument();
  expect(mockWsClient.call).not.toHaveBeenCalled();
});
it('renders the admin settings table without another decision path or internal identifiers', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview,
    approval: { ...preview.approval, kind: 'admin_ai_settings', payload: {
      id: 'PRIVATE-ID', admin: 'Owner', avatar: '', employee_count: 1, item_count: 1,
      items: [{ seq: 1, target: 'PRIVATE-TARGET', employee: 'Nova', label: 'Setting name', before: 'Old value', after: 'New value' }],
      reason: 'A readable reason', expires_at: null,
    } },
  }));
  page();
  expect(await screen.findByRole('table')).toHaveTextContent('Setting name');
  expect(screen.getByRole('table')).toHaveTextContent('Old value');
  expect(screen.getByRole('table')).toHaveTextContent('New value');
  expect(document.body.textContent).not.toContain('PRIVATE-ID');
  expect(document.body.textContent).not.toContain('PRIVATE-TARGET');
  expect(screen.getAllByRole('button')).toHaveLength(2);
});
