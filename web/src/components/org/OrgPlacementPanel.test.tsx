import { mockWsClient } from '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { toast } from '@/lib/toast';
import type { OrgPendingChange } from '@/lib/api';
import { OrgPlacementPanel, type PlacementAgent } from './OrgPlacementPanel';

const agents: PlacementAgent[] = [
  { name: 'dudu', display_name: 'Dudu', role: 'main' },
  { name: 'sales', display_name: 'Sam', role: 'specialist', department: 'Sales', reports_to: 'dudu' },
  { name: 'quote', display_name: 'Quote bot', role: 'worker', department: 'Sales', reports_to: 'dudu' },
  { name: 'track', display_name: 'Tracker', role: 'worker', department: 'Sales', reports_to: 'quote' },
];
const quote = agents[2];

type Handler = (params: Record<string, unknown>) => unknown;

function route(handlers: Record<string, Handler>) {
  mockWsClient.call.mockImplementation(async (method: string, params: Record<string, unknown>) => {
    const h = handlers[method];
    if (!h) return {};
    return h(params);
  });
}

const placement = (reports_to: string, department: string, base = 'b1') => () => ({
  agent_id: 'quote', placement: { reports_to, department }, source: 'authority', mirror_drift: false, base,
});
const departments = () => ({ departments: ['Sales', 'Support'].map((name) => ({ name, agent_count: 1, members: [], wiki_pages: 0, skills: 0 })) });

function calls(method: string) {
  return mockWsClient.call.mock.calls.filter(([m]) => m === method);
}

function renderPanel(props: Partial<React.ComponentProps<typeof OrgPlacementPanel>> = {}) {
  const onChanged = vi.fn();
  const onRefresh = vi.fn();
  renderWithProviders(
    <OrgPlacementPanel agent={quote} agents={agents} isAdmin onChanged={onChanged} onRefresh={onRefresh} {...props} />,
  );
  return { onChanged, onRefresh };
}

const pick = (label: string, value: string) =>
  fireEvent.change(screen.getByLabelText(label), { target: { value } });

beforeEach(() => {
  vi.clearAllMocks();
  vi.spyOn(toast, 'success').mockImplementation(() => undefined);
  vi.spyOn(toast, 'error').mockImplementation(() => undefined);
  route({ 'org.placement': placement('dudu', 'Sales'), 'departments.list': departments });
});

describe('<OrgPlacementPanel> admin', () => {
  it('shows the current value and applies after the before/after confirmation', async () => {
    route({
      'org.placement': placement('dudu', 'Sales'),
      'departments.list': departments,
      'org.apply_change': () => ({
        status: 'applied', agent_id: 'quote', before: { reports_to: 'dudu', department: 'Sales' },
        after: { reports_to: 'sales', department: 'Sales' }, base: 'b2', registry_reloaded: true, withdrawn_pending: [],
      }),
    });
    const { onChanged } = renderPanel();
    await waitFor(() => expect(calls('org.placement')).toHaveLength(1));

    expect(screen.getByRole('button', { name: 'Apply change' })).toBeDisabled();
    pick('Manager', 'sales');
    expect(screen.getByText('Currently: Dudu')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Apply change' }));

    // Nothing is sent before the admin confirms; the dialog re-reads the base.
    expect(calls('org.apply_change')).toHaveLength(0);
    const after = await screen.findByTestId('org-confirm-after');
    expect(within(screen.getByTestId('org-confirm-before')).getByText('Dudu')).toBeInTheDocument();
    expect(within(after).getByText('Sam')).toBeInTheDocument();
    expect(within(after).getByText('(unchanged)')).toBeInTheDocument();
    await waitFor(() => expect(calls('org.placement')).toHaveLength(2));

    fireEvent.click(screen.getByRole('button', { name: 'Confirm change' }));
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
    expect(calls('org.apply_change')[0][1]).toEqual({ agent_id: 'quote', reports_to: 'sales', expected_base: 'b1' });
    expect(toast.success).toHaveBeenCalledWith('Quote bot now reports to Sam');
  });

  it('reloads and tells the admin when the org changed after the dialog loaded', async () => {
    route({
      'org.placement': placement('dudu', 'Sales'),
      'departments.list': departments,
      'org.apply_change': () => {
        throw { code: 'stale_base', message: 'moved', current: { reports_to: 'track', department: 'Sales' }, current_base: 'b9' };
      },
    });
    const { onChanged } = renderPanel();
    await waitFor(() => expect(calls('org.placement')).toHaveLength(1));
    pick('Manager', 'sales');
    fireEvent.click(screen.getByRole('button', { name: 'Apply change' }));
    await screen.findByTestId('org-confirm-after');
    fireEvent.click(screen.getByRole('button', { name: 'Confirm change' }));

    const alert = await screen.findByText(/changed in the meantime/);
    expect(alert).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalled();
    expect(screen.getByText('Currently: Tracker')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByTestId('org-confirm-after')).not.toBeInTheDocument());
    // The admin can review and apply again against the new base.
    await waitFor(() => expect(screen.getByRole('button', { name: 'Apply change' })).toBeEnabled());
  });

  it('says the change is saved and retries the employee settings sync', async () => {
    let first = true;
    route({
      'org.placement': placement('dudu', 'Sales'),
      'departments.list': departments,
      'org.apply_change': (params) => {
        if (first) {
          first = false;
          throw { code: 'mirror_sync_failed', message: 'disk', applied: true, base: 'b2' };
        }
        return { status: 'mirror_synced', agent_id: 'quote', placement: { reports_to: 'sales', department: 'Sales' }, base: params.expected_base, registry_reloaded: true };
      },
    });
    const { onChanged } = renderPanel();
    await waitFor(() => expect(calls('org.placement')).toHaveLength(1));
    pick('Manager', 'sales');
    fireEvent.click(screen.getByRole('button', { name: 'Apply change' }));
    await screen.findByTestId('org-confirm-after');
    fireEvent.click(screen.getByRole('button', { name: 'Confirm change' }));

    expect(await screen.findByText(/The change is saved/)).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalled();
    route({
      'org.placement': placement('sales', 'Sales', 'b2'),
      'departments.list': departments,
      'org.apply_change': (params) => ({ status: 'mirror_synced', agent_id: 'quote', placement: { reports_to: 'sales', department: 'Sales' }, base: params.expected_base, registry_reloaded: true }),
    });
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Employee settings file re-synced'));
    expect(calls('org.apply_change').at(-1)![1]).toEqual({ agent_id: 'quote', reports_to: 'sales', department: 'Sales', expected_base: 'b2' });
    expect(screen.queryByText(/The change is saved/)).not.toBeInTheDocument();
  });
});

describe('<OrgPlacementPanel> manager', () => {
  it('explains approval and submits a request instead of applying', async () => {
    route({
      'org.placement': placement('dudu', 'Sales'),
      'departments.list': departments,
      'org.propose_change': () => ({ status: 'pending_approval', approval_id: 'a1', agent_id: 'quote', expires_at: null }),
    });
    const { onChanged } = renderPanel({ isAdmin: false });
    await waitFor(() => expect(calls('org.placement')).toHaveLength(1));
    expect(screen.getByText(/takes effect only after an administrator approves/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Apply change' })).not.toBeInTheDocument();

    pick('Manager', 'sales');
    fireEvent.click(screen.getByRole('button', { name: 'Submit request' }));
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
    expect(calls('org.propose_change')[0][1]).toEqual({ agent_id: 'quote', reports_to: 'sales' });
    expect(calls('org.apply_change')).toHaveLength(0);
    expect(toast.success).toHaveBeenCalledWith('Request submitted. It takes effect once an administrator approves it.');
  });

  it('blocks a second request while one is waiting', async () => {
    const pending = { approval_id: 'a1', agent_id: 'quote', stale: false } as OrgPendingChange;
    renderPanel({ isAdmin: false, pending });
    await waitFor(() => expect(calls('org.placement')).toHaveLength(1));
    pick('Manager', 'sales');
    expect(screen.getByRole('button', { name: 'Submit request' })).toBeDisabled();
    expect(screen.getByRole('note')).toHaveTextContent('already has a change waiting for approval');
  });
});

describe('<OrgPlacementPanel> errors beside the field (B5)', () => {
  it('flags a reporting cycle on the manager field and disables apply', async () => {
    renderPanel();
    await waitFor(() => expect(calls('org.placement')).toHaveLength(1));
    pick('Manager', 'track'); // Tracker reports to Quote bot
    const select = screen.getByLabelText('Manager');
    expect(select).toHaveAttribute('aria-invalid', 'true');
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('That would create a loop');
    expect(select.getAttribute('aria-describedby')).toBe(alert.id);
    expect(screen.getByRole('button', { name: 'Apply change' })).toBeDisabled();
    // Choosing something acceptable clears it.
    pick('Manager', 'sales');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Apply change' })).toBeEnabled();
  });

  async function applyRejected(error: unknown, change: () => void) {
    route({
      'org.placement': placement('dudu', 'Sales'),
      'departments.list': departments,
      'org.apply_change': () => { throw error; },
    });
    const view = renderPanel();
    await screen.findByRole('option', { name: 'Support' });
    await waitFor(() => expect(calls('org.placement')).toHaveLength(1));
    change();
    fireEvent.click(screen.getByRole('button', { name: 'Apply change' }));
    await screen.findByTestId('org-confirm-after');
    fireEvent.click(screen.getByRole('button', { name: 'Confirm change' }));
    return view;
  }

  it('shows a missing manager next to the field and offers a refresh', async () => {
    const { onRefresh } = await applyRejected({ code: 'target_not_found', message: 'gone' }, () => pick('Manager', 'sales'));
    const alert = await screen.findByText(/This manager could not be found/);
    expect(alert).toBeInTheDocument();
    expect(screen.getByLabelText('Manager')).toHaveAttribute('aria-invalid', 'true');
    await waitFor(() => expect(screen.getByRole('button', { name: 'Apply change' })).toBeDisabled());
    fireEvent.click(await screen.findByRole('button', { name: 'Refresh' }));
    expect(onRefresh).toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByText(/This manager could not be found/)).not.toBeInTheDocument());
  });

  it('shows a missing department next to the department field', async () => {
    await applyRejected({ code: 'unknown_department', message: 'nope' }, () => pick('Department', 'Support'));
    await screen.findByText(/This department does not exist/);
    expect(screen.getByLabelText('Department')).toHaveAttribute('aria-invalid', 'true');
    await waitFor(() => expect(screen.getByRole('button', { name: 'Apply change' })).toBeDisabled());
  });

  it('says organization data is unavailable and keeps apply disabled', async () => {
    route({
      'org.placement': () => { throw { code: 'org_authority_unavailable', message: 'locked' }; },
      'departments.list': departments,
    });
    renderPanel();
    expect(await screen.findByText('Organization data is temporarily unavailable. Please contact an administrator.')).toBeInTheDocument();
    pick('Manager', 'sales');
    expect(screen.getByRole('button', { name: 'Apply change' })).toBeDisabled();
  });
});
