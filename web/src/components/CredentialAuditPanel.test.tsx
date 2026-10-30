import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import type { ChatCredential } from '@/lib/api';
import { CredentialAuditPanel, credentialCsvCell } from './CredentialAuditPanel';

const credential: ChatCredential = {
  id: 'cred_fixture', kind: 'password', account_hint: 'a***@example.com',
  service: 'Example mail', service_source: 'detected', source_kind: 'chat',
  source_ref: 'webchat:session:42', provided_by: 'user-fixture', status: 'active',
  first_seen_at: '2026-10-30T01:00:00Z', last_seen_at: '2026-10-30T01:00:00Z',
  last_used_at: null, note: null, holders: [{ holder_kind: 'agent', holder_id: 'support-fixture' }],
};

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockImplementation((method: string) => Promise.resolve(
    method === 'credentials.audit_list' ? { credentials: [credential], mode: 'audit' } : { success: true },
  ));
});

describe('CredentialAuditPanel', () => {
  it('shows masked metadata and explicitly identifies plaintext history in audit mode', async () => {
    renderWithProviders(<CredentialAuditPanel />);
    expect(await screen.findByText('a***@example.com')).toBeInTheDocument();
    expect(screen.getByRole('note')).toHaveTextContent('plaintext remains in conversation history');
    expect(screen.getByText('user-fixture')).toBeInTheDocument();
    expect(screen.getByText('AI employee: support-fixture')).toBeInTheDocument();
    expect(screen.getByText('Not observed')).toBeInTheDocument();
    expect(screen.getByText('webchat:session:42')).toBeInTheDocument();
  });

  it('does not present revoked holders as current holders', async () => {
    mockWsClient.call.mockResolvedValue({ credentials: [{ ...credential, holders: [
      { holder_kind: 'agent', holder_id: 'former-holder', revoked_at: '2026-10-30T02:00:00Z' },
    ] }], mode: 'audit' });
    renderWithProviders(<CredentialAuditPanel />);
    await screen.findByText('a***@example.com');
    expect(screen.queryByText(/former-holder/)).not.toBeInTheDocument();
  });

  it('does not promise that mask mode rewrites earlier conversation history', async () => {
    mockWsClient.call.mockResolvedValue({ credentials: [], mode: 'mask' });
    renderWithProviders(<CredentialAuditPanel />);
    expect(await screen.findByRole('note')).toHaveTextContent('Earlier conversation history is unchanged');
    expect(screen.getByText('No matching credentials')).toBeInTheDocument();
  });

  it('sends filters to the gateway and resets to the first page', async () => {
    renderWithProviders(<CredentialAuditPanel />);
    await screen.findByText('a***@example.com');
    await userEvent.selectOptions(screen.getByLabelText('Type'), 'api_key');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('credentials.audit_list', {
      kind: 'api_key', limit: 100, offset: 0,
    }));
  });

  it('marks a finding false positive using only its id and status', async () => {
    renderWithProviders(<CredentialAuditPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Mark false positive' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('credentials.audit_update', {
      id: 'cred_fixture', status: 'false_positive',
    });
  });

  it('edits the service without submitting a credential value', async () => {
    renderWithProviders(<CredentialAuditPanel />);
    await userEvent.click(await screen.findByRole('button', { name: 'Edit service' }));
    const input = screen.getByLabelText('Service name (no credentials)');
    await userEvent.clear(input);
    await userEvent.type(input, 'Support mailbox');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('credentials.audit_update', {
      id: 'cred_fixture', service: 'Support mailbox',
    });
  });

  it('reports load failure instead of showing an empty inventory', async () => {
    mockWsClient.call.mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<CredentialAuditPanel />);
    expect(await screen.findByText('Could not complete the credential audit request')).toBeInTheDocument();
    expect(screen.queryByText('No matching credentials')).not.toBeInTheDocument();
  });

  it('requests an audited bounded export and reports refusal before downloading', async () => {
    mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
      if (method === 'credentials.audit_list' && params?.export) return Promise.reject(new Error('audit unavailable'));
      return Promise.resolve({ credentials: [credential], mode: 'audit' });
    });
    renderWithProviders(<CredentialAuditPanel />);
    await screen.findByText('a***@example.com');
    await userEvent.click(screen.getByRole('button', { name: 'Export this page as CSV' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('credentials.audit_list', {
      limit: 100, offset: 0, export: true,
    });
    expect(await screen.findByText('Could not complete the credential audit request')).toBeInTheDocument();
  });

  // F6 regression tests; not run (owner forbids builds and npm).
  it('labels only the current segment as internally consistent', async () => {
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'security.audit_verify'
      ? { status: 'current_segment_consistent', scope: 'current_segment', intact: true, lines_checked: 2 }
      : { credentials: [credential], mode: 'audit' }));
    renderWithProviders(<CredentialAuditPanel />);
    await userEvent.click(screen.getByRole('button', { name: 'Verify current audit segment' }));
    expect(await screen.findByText('Current segment internally consistent')).toBeInTheDocument();
    expect(screen.getByText(/Rotation verifies only the current segment/)).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('security.audit_verify');
  });

  it('does not display a consistency claim for broken or incomplete verification', async () => {
    mockWsClient.call.mockImplementation((method: string) => Promise.resolve(method === 'security.audit_verify'
      ? { status: 'broken', scope: 'current_segment', intact: false, line_number: 2 }
      : { credentials: [credential], mode: 'audit' }));
    renderWithProviders(<CredentialAuditPanel />);
    await userEvent.click(screen.getByRole('button', { name: 'Verify current audit segment' }));
    expect(await screen.findByText('Current segment contains a broken or malformed audit row')).toBeInTheDocument();
    expect(screen.queryByText('Current segment internally consistent')).not.toBeInTheDocument();
    mockWsClient.call.mockResolvedValue({ status: 'current_segment_consistent', scope: 'current_segment', intact: true });
    await userEvent.click(screen.getByRole('button', { name: 'Verify current audit segment' }));
    expect(await screen.findByText('No current segment consistency result is available')).toBeInTheDocument();
  });

  it('paginates without exporting unseen rows', async () => {
    mockWsClient.call.mockResolvedValue({
      mode: 'audit', credentials: Array.from({ length: 100 }, (_, i) => ({ ...credential, id: `cred_${i}` })),
    });
    renderWithProviders(<CredentialAuditPanel />);
    const table = await screen.findByRole('table');
    await waitFor(() => expect(within(table).getAllByRole('row')).toHaveLength(101));
    await userEvent.click(screen.getByRole('button', { name: 'Next' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('credentials.audit_list', { limit: 100, offset: 100 }));
  });
});

describe('credentialCsvCell', () => {
  it('quotes commas, quotes and multiline fields', () => {
    expect(credentialCsvCell('a,"b"\nc')).toBe('"a,""b""\nc"');
  });

  it('neutralizes formula prefixes including leading whitespace', () => {
    for (const value of ['=SUM(A1)', '+cmd', '-1', '@cmd', '\t=1', ' \r@cmd']) {
      expect(credentialCsvCell(value)).toBe(`"'${value}"`);
    }
    expect(credentialCsvCell('a***@example.com')).toBe('"a***@example.com"');
  });
});
