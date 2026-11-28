import { StrictMode } from 'react';
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route, useLocation } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import zhTW from '@/i18n/zh-TW.json';
import { mockWsClient } from '@/test/mocks';
import { ApprovalConfirmPage } from './ApprovalConfirmPage';
import { useAgentsStore } from '@/stores/agents-store';
import type { AgentDetail } from '@/lib/api';
import { approvalConfirmReturn, approvalStepUp } from '@/lib/approval-step-up';
import { useAuthStore } from '@/stores/auth-store';
import { dashboardEndpoint } from '@/lib/dashboard-base';

const ticket = 'ab'.repeat(32);
const token = Array.from({ length: 32 }, (_, i) => i);
const preview = {
  status: 'live', kind_label: 'External send', digest: 'digest-reviewed', has_totp: true,
  expires: Math.floor(Date.now() / 1000) + 600, agent_file_change: null,
  approval: { id: 'approval-private', kind: 'cli_egress', digest: 'digest-reviewed', summary: 'Send the quote', payload: { content: '<script>full email</script>' } },
};
const fetchMock = vi.fn();
function response(value: unknown, status = 200) { return { ok: status === 200, status, json: async () => value }; }
function page(hash = `#ticket=${ticket}`, messages = en) {
  window.history.replaceState(null, '', `/approvals/confirm${hash}`);
  return render(<IntlProvider locale="en" messages={messages}><MemoryRouter initialEntries={[`/approvals/confirm${hash}`]}>
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
  useAgentsStore.setState({ agents: [], fetchAgents: vi.fn().mockResolvedValue(true) });
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
  fetchMock.mockResolvedValueOnce(response({ status: 'done', decision: 'approve', decided_by: 'hidden' }));
  await approve();
  expect(await screen.findByText(/✓ Approved:/)).toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledTimes(3);
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
      stale: false, content_available: true, diff: { added: 0, removed: 0, hunks: [] } },
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

it.each([
  ['missing digest', { ...preview, digest: undefined }],
  ['empty digest', { ...preview, digest: ' ' }],
  ['wrong status', { ...preview, status: 'done' }],
  ['missing approval id', { ...preview, approval: { ...preview.approval, id: undefined } }],
  ['non-finite expiry', { ...preview, expires: Infinity }],
  ['wrong TOTP type', { ...preview, has_totp: 'true' }],
  ['empty kind label', { ...preview, kind_label: '' }],
  ['wrong kind type', { ...preview, approval: { ...preview.approval, kind: 1 } }],
  ['wrong summary type', { ...preview, approval: { ...preview.approval, summary: null } }],
  ['array file projection', { ...preview, agent_file_change: [] }],
  ['missing file projection', { ...preview, agent_file_change: undefined }],
])('fails closed for GET %s', async (_label, value) => {
  fetchMock.mockResolvedValueOnce(response(value));
  page();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledOnce();
});
it.each(['GET', 'POST'])('fails closed for a non-JSON %s body', async method => {
  const invalid = { ok: true, status: 200, json: async () => { throw new SyntaxError('not JSON'); } };
  if (method === 'POST') fetchMock.mockResolvedValueOnce(response(preview));
  fetchMock.mockResolvedValueOnce(invalid);
  page();
  if (method === 'POST') await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
  expect(screen.queryByText(/✓ Approved:/)).not.toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledTimes(method === 'POST' ? 2 : 1);
});
it.each([
  { status: 'live', decision: 'approve', decided_by: 'hidden' },
  { status: 'done', decision: 'reject', decided_by: 'hidden' },
  { status: 'done', decision: 'approve', decided_by: null },
])('never shows success or retries a malformed POST response %j', async value => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response(value));
  page(); await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByText(/✓ Approved:|✗ Rejected:/)).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(fetchMock.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(1);
});
it.each(['GET', 'POST'])('rejects malformed %s error bodies', async method => {
  if (method === 'POST') fetchMock.mockResolvedValueOnce(response(preview));
  fetchMock.mockResolvedValueOnce(response({ status: 'step_up_required', error: 42 }, 403));
  page(); if (method === 'POST') await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledTimes(method === 'POST' ? 2 : 1);
});
it.each([null, [], {}, { status: 403, error: 'Denied' }, { status: 'forbidden' }])('rejects malformed error shape %j', async value => {
  fetchMock.mockResolvedValueOnce(response(value, 403));
  page(); expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
});
it('never sends an undefined digest', async () => {
  await expect(approvalStepUp.decide(ticket, 'reject', undefined as unknown as string)).rejects.toMatchObject({ code: 'unavailable' });
  expect(fetchMock).not.toHaveBeenCalled();
});

const privateUuid = 'aabbccdd-1234-5678-9012-aabbccddeeff';
function expectNoIdentifiers() {
  for (const value of [privateUuid, 'PRIVATE-AGENT', 'PRIVATE-TARGET', 'PRIVATE-MANAGER', '{', '}']) {
    expect(document.body.textContent).not.toContain(value);
  }
  expect(document.querySelector('pre')).toBeNull();
}
it('shows org employee and manager display names without duplicating the raw payload', async () => {
  useAgentsStore.setState({ agents: [
    { name: 'PRIVATE-AGENT', display_name: 'Nova' },
    { name: 'PRIVATE-MANAGER', display_name: 'Orion' },
  ] as AgentDetail[] });
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval,
    kind: 'org_change', agent_id: 'PRIVATE-AGENT', payload: {
      id: privateUuid, target: 'PRIVATE-TARGET', agent_id: 'PRIVATE-AGENT',
      before: { reports_to: 'PRIVATE-MANAGER', department: 'Sales' },
      after: { reports_to: privateUuid, department: 'Support' }, reason: 'Change team',
    },
  } }));
  page(); await screen.findByText('Change team');
  expect(screen.getByText('Nova')).toBeInTheDocument();
  expect(screen.getByText('Orion')).toBeInTheDocument();
  expect(screen.getByText(en['approvalConfirm.unknownAgent'])).toBeInTheDocument();
  expectNoIdentifiers();
});
it.each(['expert_skills_enable', 'autopilot_action', 'cli_egress', 'egress_guard'])('renders full read-only evidence without identifiers for %s', async kind => {
  const mailBody = 'Hello team,\n' + 'This is the full mail body. '.repeat(100) + '\nEnd of message.';
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval, kind,
    agent_id: 'PRIVATE-AGENT', payload: {
      id: privateUuid, agent_id: 'PRIVATE-AGENT', target: 'sales@example.com', binding: 'hidden-binding',
      payload_hash: 'hidden-hash', digest: 'hidden-digest', hash: 'hidden-hash', agent_ids: ['PRIVATE-AGENT'],
      egress_guard: { nested: { id: 'nested-id', target: 'sales@example.com', recipient: 'team@example.com', body: mailBody } },
      entries: [{ employee_id: 'PRIVATE-AGENT', value: privateUuid }, 'visible entry'],
    },
  } }));
  page(); await screen.findByText('Send the quote');
  expect(screen.getByText((_, element) => element?.tagName === 'SPAN' && element.textContent === mailBody)).toHaveClass('whitespace-pre-wrap');
  expect(screen.getByText('team@example.com')).toBeInTheDocument();
  expect(screen.getByText('visible entry')).toBeInTheDocument();
  expect(screen.queryByText(/hidden-|nested-id/)).not.toBeInTheDocument();
  expectNoIdentifiers();
});
it('preserves outbound body text while redacting every embedded UUID', async () => {
  const secondUuid = 'ABCDEF12-3456-7890-ABCD-EF1234567890';
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval,
    payload: { body: `Please review reference ${privateUuid} before sending reference ${secondUuid} to the team.` },
  } }));
  page();
  expect(await screen.findByText('Please review reference … before sending reference … to the team.')).toBeInTheDocument();
  expect(document.body.textContent).not.toContain(privateUuid);
  expect(document.body.textContent).not.toContain(secondUuid);
});
it('filters a JSON-encoded payload before rendering evidence', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval,
    payload: JSON.stringify({ target: 'sales@example.com', agent_id: 'PRIVATE-AGENT', text: 'Readable evidence' }),
  } }));
  page(); await screen.findByText('Readable evidence'); expectNoIdentifiers();
});
it('shows the inbox fallback when filtering leaves no evidence', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval,
    payload: { agent_ids: ['PRIVATE-AGENT'] },
  } }));
  page(); expect(await screen.findByText(en['approvalConfirm.inboxContent'])).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Reject' })).not.toBeInTheDocument();
  expect(fetchMock.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(0);
  expectNoIdentifiers();
});

const fileProjection = {
  approval_id: preview.approval.id, agent_id: 'Employee', file: 'soul',
  requested_by: 'PRIVATE-PERSON', reason: 'Review file', verdict: 'pass', reason_categories: [],
  stale: false, content_available: true, diff: { added: 0, removed: 0, hunks: [] },
};
it.each([
  ['mismatched digest', { ...preview.approval, digest: 'different' }],
  ['missing approval digest', { ...preview.approval, digest: undefined }],
  ['blank approval digest', { ...preview.approval, digest: ' ' }],
  ['unknown kind', { ...preview.approval, kind: 'unknown_kind' }],
  ['null workflow projection', { ...preview.approval, kind: 'workflow_activation', workflow_activation: null }],
  ['invalid settings', { ...preview.approval, kind: 'admin_ai_settings', payload: {} }],
  ['invalid org payload', { ...preview.approval, kind: 'org_change', payload: null }],
  ['empty generic object', { ...preview.approval, payload: {} }],
  ['empty generic string', { ...preview.approval, payload: ' ' }],
])('rejects incomplete GET content: %s', async (_label, approval) => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval }));
  page();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button')).not.toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledOnce();
});
it.each([{}, { ...fileProjection, content_available: false }, { ...fileProjection, stale: true },
  { ...fileProjection, file: ' ' }, { ...fileProjection, diff: null },
])('rejects incomplete file projection %j', async projection => {
  fetchMock.mockResolvedValueOnce(response({ ...preview,
    approval: { ...preview.approval, kind: 'agent_file_change' }, agent_file_change: projection,
  }));
  page();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button')).not.toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledOnce();
});
it.each([
  [503, { status: 'step_up_required', error: 'x' }],
  [403, { status: 'verification_failed', error: 'x' }],
  [403, { status: 'unknown_code', error: 'x' }],
  [403, { status: 'step_up_required', error: ' ' }],
  [403, { status: 'step_up_required' }],
  [401, { status: 'forbidden', error: 'x' }],
])('makes invalid HTTP %s error %j terminal after POST', async (http, body) => {
  fetchMock.mockResolvedValueOnce(response(preview)).mockResolvedValueOnce(response(body, http));
  page(); await approve();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button')).not.toBeInTheDocument();
  expect(document.querySelector('form')).toBeNull();
  await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
});
it('rejects an unknown GET error code', async () => {
  fetchMock.mockResolvedValueOnce(response({ status: 'unknown_code', error: 'x' }, 403));
  page();
  expect(await screen.findByRole('alert')).toHaveTextContent(en['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button')).not.toBeInTheDocument();
});
it('hides camelCase and UUID identifier keys while preserving targets and full text', async () => {
  const body = `Dear team, ${'Mail content. '.repeat(100)}Reference ${privateUuid}. Thanks.`;
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval, payload: {
    agentId: 'HIDDEN-AGENT', workflowId: 'HIDDEN-WORKFLOW', request_uuid: 'HIDDEN-REQUEST',
    REQUESTIDS: ['HIDDEN-IDS'], 'Payload-Hash': 'HIDDEN-HASH', UUID: 'HIDDEN-UUID',
    target: 'sales@example.com', body, reference: privateUuid, path: `/files/${privateUuid}`,
  } } }));
  page();
  expect(await screen.findByText('sales@example.com')).toBeInTheDocument();
  expect(screen.getByText((_, el) => el?.tagName === 'SPAN' && el.textContent === body.replace(privateUuid, '…'))).toBeInTheDocument();
  expect(screen.getByText('…')).toBeInTheDocument();
  expect(screen.getByText('/files/…')).toBeInTheDocument();
  expect(document.body.textContent).not.toMatch(/HIDDEN-|aabbccdd/);
});
it('shows an unknown member for an org proposer without email', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval,
    kind: 'org_change', payload: { agent_id: 'Employee', proposer: 'PRIVATE-PERSON', reason: 'Change team',
      before: { reports_to: '', department: '' }, after: { reports_to: '', department: 'Support' } },
  } }));
  page(undefined, zhTW);
  expect(await screen.findByText('未知的成員')).toBeInTheDocument();
  expect(document.body.textContent).not.toContain('PRIVATE-PERSON');
});

const workflowFacts = {
  workflow_id: 'workflow-private', revision: 1,
  effect_targets: [{ step_id: 'send', tool: 'mail.send', param: null, target: 'team@example.com' }],
  expires_at: null, submitted_by: 'Owner',
  pricing: { money_limits_effective: true, limits: {
    max_runs_per_month: 10, max_steps_per_run: 5, max_reads_per_run: 5, max_effects_per_run: 1,
  } },
};
const orgPayload = {
  agent_id: 'Employee', before: { reports_to: '', department: '' },
  after: { reports_to: 'Manager', department: 'Support' }, reason: 'Change team',
};
it.each([
  ['empty workflow', { kind: 'workflow_activation', workflow_activation: {} }],
  ['workflow target missing tool', { kind: 'workflow_activation', workflow_activation: {
    ...workflowFacts, effect_targets: [{ step_id: 'send', param: null, target: null }],
  } }],
  ['workflow invalid pricing', { kind: 'workflow_activation', workflow_activation: { ...workflowFacts, pricing: {} } }],
  ['workflow invalid expiry', { kind: 'workflow_activation', workflow_activation: { ...workflowFacts, expires_at: 1 } }],
  ['empty org payload', { kind: 'org_change', payload: {} }],
  ['malformed org after', { kind: 'org_change', payload: { ...orgPayload, after: { reports_to: 1, department: 'Support' } } }],
  ['missing org before', { kind: 'org_change', payload: { ...orgPayload, before: undefined } }],
  ['invalid org reason', { kind: 'org_change', payload: { ...orgPayload, reason: null } }],
])('makes missing review content terminal: %s', async (_label, approval) => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval, ...approval } }));
  page(undefined, zhTW);
  expect(await screen.findByRole('alert')).toHaveTextContent(zhTW['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button', { name: '核准' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '拒絕' })).not.toBeInTheDocument();
  expect(screen.getByRole('link')).toHaveAttribute('href', '/inbox');
  expect(fetchMock.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(0);
});
it.each([
  ['empty diff', { diff: {} }],
  ['missing hunks', { diff: { added: 1, removed: 0 } }],
  ['missing lines', { diff: { added: 1, removed: 0, hunks: [{ op: 'add' }] } }],
  ['non-string line', { diff: { added: 1, removed: 0, hunks: [{ op: 'add', lines: [42] }] } }],
  ['invalid operation', { diff: { added: 1, removed: 0, hunks: [{ op: 'insert', lines: ['text'] }] } }],
  ['invalid count', { diff: { added: '1', removed: 0, hunks: [] } }],
  ['mismatched approval', { approval_id: 'another-approval' }],
  ['missing requester', { requested_by: undefined }],
  ['invalid reason', { reason: null }],
])('makes file review content terminal: %s', async (_label, fields) => {
  fetchMock.mockResolvedValueOnce(response({ ...preview,
    approval: { ...preview.approval, kind: 'agent_file_change' },
    agent_file_change: { ...fileProjection, ...fields },
  }));
  page(undefined, zhTW);
  expect(await screen.findByRole('alert')).toHaveTextContent(zhTW['approvalConfirm.unavailable']);
  expect(screen.queryByRole('button', { name: '核准' })).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: '拒絕' })).not.toBeInTheDocument();
  expect(fetchMock.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(0);
});
it.each(['expert_skills_enable', 'autopilot_action', 'cli_egress', 'egress_guard'])('blocks decisions for filtered-only %s evidence', async kind => {
  fetchMock.mockResolvedValueOnce(response({ ...preview,
    approval: { ...preview.approval, kind, payload: { agent_ids: ['x'] } },
  }));
  page(undefined, zhTW);
  expect(await screen.findByRole('alert')).toHaveTextContent('這類審批的完整內容請到儀表板收件匣查看。');
  expect(screen.queryByRole('button')).not.toBeInTheDocument();
  expect(screen.queryByText('Send the quote')).not.toBeInTheDocument();
  expect(screen.getByRole('link')).toHaveAttribute('href', '/inbox');
  expect(fetchMock.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(0);
});
it.each([
  { kind: 'workflow_activation', workflow_activation: workflowFacts },
  { kind: 'org_change', payload: orgPayload },
  { kind: 'org_change', payload: { ...orgPayload, before: JSON.stringify(orgPayload.before), after: JSON.stringify(orgPayload.after) } },
  { kind: 'agent_file_change' },
  { kind: 'admin_ai_settings', payload: {
    id: 'settings', admin: 'Owner', avatar: '', employee_count: 1, item_count: 1,
    items: [{ seq: 1, target: 'setting', employee: 'Nova', label: 'Setting', before: 'Old', after: 'New' }],
    reason: 'Review settings', expires_at: null,
  } },
  ...['expert_skills_enable', 'autopilot_action', 'cli_egress', 'egress_guard']
    .map(kind => ({ kind, payload: { body: 'Reviewable content' } })),
])('keeps valid $kind review decisions available', async approval => {
  fetchMock.mockResolvedValueOnce(response({ ...preview,
    approval: { ...preview.approval, ...approval },
    agent_file_change: approval.kind === 'agent_file_change' ? { ...fileProjection,
      diff: { added: 1, removed: 1, hunks: [
        { op: 'equal', lines: ['Context'] }, { op: 'remove', lines: ['Old'] }, { op: 'add', lines: ['New'] },
      ] },
    } : null,
  }));
  page(undefined, zhTW);
  expect(await screen.findByRole('button', { name: '核准' })).toBeInTheDocument();
  expect(screen.getByRole('button', { name: '拒絕' })).toBeInTheDocument();
});
it('shows an unknown member for an id-like file requester', async () => {
  fetchMock.mockResolvedValueOnce(response({ ...preview, approval: { ...preview.approval, kind: 'agent_file_change' },
    agent_file_change: fileProjection,
  }));
  page(undefined, zhTW);
  expect(await screen.findByText('未知的成員')).toBeInTheDocument();
  expect(document.body.textContent).not.toContain('PRIVATE-PERSON');
});
