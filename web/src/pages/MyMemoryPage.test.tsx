import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';
import { toast } from '@/lib/toast';
import { MyMemoryPage, myMemoryErrorCode } from './MyMemoryPage';

const NOT_FOUND = '找不到這筆記憶，可能已被刪除。';

const item = (over: Record<string, unknown> = {}) => ({
  id: 'm1',
  agent: 'quote-bot',
  text: 'Regional discount cap is 8%.',
  created_at: '2026-11-05T14:03:00Z',
  source_kind: 'conversation',
  explicit: false,
  ...over,
});

const PAGE_ONE = {
  items: [
    item(),
    item({ id: 'm2', agent: 'duduclaw', text: 'No non-urgent pings after 9pm.', source_kind: 'explicit', explicit: true }),
    item({ id: 'm3', agent: 'quote-bot', text: 'Q1 planning folder is on the shared drive.', source_kind: 'footprint' }),
  ],
  next_cursor: 'cursor-2',
};

type Handler = (method: string, params?: Record<string, unknown>) => unknown;
function serve(handler: Handler) {
  mockWsClient.call.mockImplementation(async (method: string, params?: Record<string, unknown>) => handler(method, params));
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockReset();
  useAgentsStore.setState({
    agents: [{ name: 'quote-bot', display_name: 'Quote Bot' }, { name: 'duduclaw', display_name: 'DuDu' }] as never,
  });
});

describe('MyMemoryPage', () => {
  it('lists memories with employee, time and plain-language source tags', async () => {
    serve(() => PAGE_ONE);
    renderWithProviders(<MyMemoryPage />);
    expect(await screen.findByText('Regional discount cap is 8%.')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('me.memory.list', { limit: 20 });

    const rows = within(screen.getByRole('list', { name: 'My memories' })).getAllByRole('listitem');
    expect(rows).toHaveLength(3);
    expect(within(rows[0]).getByText('Quote Bot')).toBeInTheDocument();
    expect(within(rows[0]).getByText('Conversation')).toBeInTheDocument();
    expect(within(rows[1]).getByText('DuDu')).toBeInTheDocument();
    expect(within(rows[1]).getByText('Asked to remember')).toBeInTheDocument();
    expect(within(rows[2]).getByText('Other')).toBeInTheDocument(); // footprint → other
    expect(screen.getByTestId('my-memory-shown')).toHaveTextContent('Showing 3 memories');
    // Every delete control is a ≥44px touch target on mobile.
    for (const button of screen.getAllByRole('button', { name: 'Delete this memory' })) {
      expect(button.className).toContain('size-11');
    }
  });

  it('loads the next page with the cursor and hides the button on the last page', async () => {
    const user = userEvent.setup();
    serve((_m, params) => params?.cursor
      ? { items: [item({ id: 'm4', text: 'Older fact.' })], next_cursor: null }
      : PAGE_ONE);
    renderWithProviders(<MyMemoryPage />);
    await screen.findByText('Regional discount cap is 8%.');

    await user.click(screen.getByRole('button', { name: 'Load more' }));
    expect(await screen.findByText('Older fact.')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenLastCalledWith('me.memory.list', { cursor: 'cursor-2', limit: 20 });
    expect(screen.getByTestId('my-memory-shown')).toHaveTextContent('Showing 4 memories');
    expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument();
  });

  it('filters by source and by AI employee', async () => {
    const user = userEvent.setup();
    serve(() => PAGE_ONE);
    renderWithProviders(<MyMemoryPage />);
    await screen.findByText('Regional discount cap is 8%.');

    await user.selectOptions(screen.getByLabelText('Filter by source'), 'explicit');
    expect(screen.getByText('No non-urgent pings after 9pm.')).toBeInTheDocument();
    expect(screen.queryByText('Regional discount cap is 8%.')).not.toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText('Filter by source'), 'all');
    await user.selectOptions(screen.getByLabelText('Filter by AI employee'), 'quote-bot');
    expect(screen.queryByText('No non-urgent pings after 9pm.')).not.toBeInTheDocument();
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
  });

  it('asks for confirmation, then forgets the memory and removes it from the list', async () => {
    const user = userEvent.setup();
    serve((method, params) => method === 'me.memory.forget'
      ? { forgotten: true, id: params?.id, agent: 'quote-bot' }
      : PAGE_ONE);
    renderWithProviders(<MyMemoryPage />);
    await screen.findByText('Regional discount cap is 8%.');

    await user.click(screen.getAllByRole('button', { name: 'Delete this memory' })[0]);
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Delete this memory?')).toBeInTheDocument();
    expect(within(dialog).getByText(/will not automatically learn this again/)).toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('me.memory.forget', expect.anything());

    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.memory.forget', { id: 'm1' }));
    await waitFor(() => expect(screen.queryByText('Regional discount cap is 8%.')).not.toBeInTheDocument());
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
  });

  it('does not forget anything when the confirmation is cancelled', async () => {
    const user = userEvent.setup();
    serve(() => PAGE_ONE);
    renderWithProviders(<MyMemoryPage />);
    await screen.findByText('Regional discount cap is 8%.');

    await user.click(screen.getAllByRole('button', { name: 'Delete this memory' })[0]);
    await user.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(mockWsClient.call).not.toHaveBeenCalledWith('me.memory.forget', expect.anything());
    expect(screen.getByText('Regional discount cap is 8%.')).toBeInTheDocument();
  });

  it('keeps the row when forgetting fails, but drops one that is already gone', async () => {
    const user = userEvent.setup();
    const info = vi.spyOn(toast, 'info');
    const error = vi.spyOn(toast, 'error');
    let failure: unknown = new Error('boom');
    serve((method) => {
      if (method === 'me.memory.forget') throw failure;
      return PAGE_ONE;
    });
    renderWithProviders(<MyMemoryPage />);
    await screen.findByText('Regional discount cap is 8%.');
    const forgetFirst = async () => {
      await user.click(screen.getAllByRole('button', { name: 'Delete this memory' })[0]);
      await user.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Delete' }));
      await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    };

    await forgetFirst();
    expect(screen.getByText('Regional discount cap is 8%.')).toBeInTheDocument();

    // The zh-TW text alone is no longer string-matched: only the code counts.
    failure = new Error(NOT_FOUND);
    await forgetFirst();
    expect(screen.getByText('Regional discount cap is 8%.')).toBeInTheDocument();

    // A coded failure gets the UI's own (here English) copy.
    failure = { code: 'memory_forget_failed', message: '刪除記憶失敗，請稍後再試。' };
    await forgetFirst();
    expect(error).toHaveBeenLastCalledWith('This memory could not be deleted. Please try again later.');
    expect(screen.getByText('Regional discount cap is 8%.')).toBeInTheDocument();

    failure = { code: 'memory_not_found', message: NOT_FOUND };
    await forgetFirst();
    await waitFor(() => expect(screen.queryByText('Regional discount cap is 8%.')).not.toBeInTheDocument());
    expect(info).toHaveBeenLastCalledWith('This memory could not be found. It may already have been deleted.');
  });

  it('reads error codes only from structured rejections', () => {
    expect(myMemoryErrorCode({ code: 'memory_not_found', message: 'x' })).toBe('memory_not_found');
    expect(myMemoryErrorCode({ code: 'something_else' })).toBeNull();
    expect(myMemoryErrorCode(new Error(NOT_FOUND))).toBeNull();
    expect(myMemoryErrorCode(NOT_FOUND)).toBeNull();
    expect(myMemoryErrorCode(null)).toBeNull();
  });

  it('shows the empty state', async () => {
    serve(() => ({ items: [], next_cursor: null }));
    renderWithProviders(<MyMemoryPage />);
    expect(await screen.findByText('No memories about you yet')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Filter by source')).not.toBeInTheDocument();
  });

  it('shows a loading skeleton while the first page is in flight', async () => {
    let finish!: (v: unknown) => void;
    mockWsClient.call.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    renderWithProviders(<MyMemoryPage />);
    expect(screen.getByLabelText('Loading your memories')).toHaveAttribute('aria-busy', 'true');
    finish({ items: [], next_cursor: null });
    expect(await screen.findByText('No memories about you yet')).toBeInTheDocument();
  });

  it('shows a retryable error state', async () => {
    const user = userEvent.setup();
    let fail = true;
    serve(() => {
      if (fail) throw new Error('down');
      return PAGE_ONE;
    });
    renderWithProviders(<MyMemoryPage />);
    expect(await screen.findByRole('alert')).toHaveTextContent("Can't load your memories right now");

    fail = false;
    await user.click(screen.getByRole('button', { name: 'Try again' }));
    expect(await screen.findByText('Regional discount cap is 8%.')).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});
