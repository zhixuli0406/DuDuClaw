import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { InboxList, type InboxGroup } from './InboxList';
import type { InboxRowLabels } from './InboxRow';
import type { InboxItem } from '@/lib/inbox-model';

const items: InboxItem[] = [
  { id: 'approval:1', type: 'approval', title: 'Approve the deploy', urgency: 20, actionable: true, timestamp: '2026-01-01T00:00:00Z' },
  { id: 'blocked:2', type: 'blocked', title: 'Task is blocked', urgency: 30, actionable: true, timestamp: '2026-01-02T00:00:00Z' },
];

const labels: InboxRowLabels = {
  typeLabel: (i) => i.type,
  riskLabel: (level) => level,
  archive: 'Archive',
  nearExpiry: 'Expiring soon',
  nearExpiryTooltip: 'Left undecided past the deadline, this is auto-rejected',
  processedTooltip: 'Processed',
};

function baseProps(over?: Partial<React.ComponentProps<typeof InboxList>>) {
  return {
    // No label ⇒ no header row, so the keyboard cursor lands on a real row.
    groups: [{ key: '', items }] as InboxGroup[],
    canArchive: true,
    agentName: (id: string) => id,
    labels,
    selectedId: null as string | null,
    isUnread: () => true,
    isProcessed: () => false,
    emptyState: <div>empty</div>,
    onSelect: vi.fn(),
    onArchive: vi.fn(),
    onUnread: vi.fn(),
    onUndo: vi.fn(),
    ...over,
  };
}

describe('<InboxList>', () => {
  it('renders every item title', () => {
    renderWithProviders(<InboxList {...baseProps()} />);
    expect(screen.getByText('Approve the deploy')).toBeInTheDocument();
    expect(screen.getByText('Task is blocked')).toBeInTheDocument();
  });

  it('shows the empty state when there are no items', () => {
    renderWithProviders(<InboxList {...baseProps({ groups: [] })} />);
    expect(screen.getByText('empty')).toBeInTheDocument();
  });

  it('clicking a row selects it (opens the detail pane)', () => {
    const props = baseProps();
    renderWithProviders(<InboxList {...props} />);
    fireEvent.click(screen.getByText('Task is blocked'));
    expect(props.onSelect).toHaveBeenCalled();
  });

  it('hover archive button fires the archive handler', () => {
    const props = baseProps();
    renderWithProviders(<InboxList {...props} />);
    fireEvent.click(screen.getAllByLabelText('Archive')[0]);
    expect(props.onArchive).toHaveBeenCalled();
  });

  it('archive button stays visible on touch (coarse pointer, no hover)', () => {
    // Hover-only reveal is unreachable on touch, so the row action must be
    // pinned visible under `pointer: coarse` (WP5.3 mobile pass).
    renderWithProviders(<InboxList {...baseProps()} />);
    expect(screen.getAllByLabelText('Archive')[0]).toHaveClass(
      'pointer-coarse:opacity-100'
    );
  });

  it('keyboard: j/k move selection, a archives, U marks unread, ⌘Z undoes', () => {
    const props = baseProps({ selectedId: 'approval:1' });
    renderWithProviders(<InboxList {...props} />);
    const listbox = screen.getByRole('listbox');
    fireEvent.keyDown(listbox, { key: 'j' });
    expect(props.onSelect).toHaveBeenCalled();
    fireEvent.keyDown(listbox, { key: 'a' });
    expect(props.onArchive).toHaveBeenCalled();
    fireEvent.keyDown(listbox, { key: 'U' });
    expect(props.onUnread).toHaveBeenCalled();
    fireEvent.keyDown(listbox, { key: 'z', metaKey: true });
    expect(props.onUndo).toHaveBeenCalled();
  });

  it('a does not archive when canArchive is false', () => {
    const props = baseProps({ canArchive: false, selectedId: 'approval:1' });
    renderWithProviders(<InboxList {...props} />);
    const listbox = screen.getByRole('listbox');
    fireEvent.keyDown(listbox, { key: 'a' });
    expect(props.onArchive).not.toHaveBeenCalled();
  });

  // ── §C6 processed axis ──────────────────────────────────────────────────
  it('a processed row shows the checkmark badge but stays in the list (never hidden)', () => {
    renderWithProviders(<InboxList {...baseProps({ isProcessed: (i) => i.id === 'blocked:2' })} />);
    expect(screen.getByText('Task is blocked')).toBeInTheDocument();
    const row = screen.getByText('Task is blocked').closest('[role="option"]') as HTMLElement;
    expect(within(row).getByLabelText('Processed')).toBeInTheDocument();
    // The unprocessed row carries no such badge.
    const otherRow = screen.getByText('Approve the deploy').closest('[role="option"]') as HTMLElement;
    expect(within(otherRow).queryByLabelText('Processed')).not.toBeInTheDocument();
  });

  // ── approval TTL countdown ──────────────────────────────────────────────
  it('shows a plain countdown well inside the window, and the amber marker only in the last third', () => {
    const now = Date.now();
    const nowSec = Math.floor(now / 1000);
    // Deliberately not right at the 1/3 boundary (that exact math is already
    // covered, with an injected clock, by `expiryState`'s own unit tests) —
    // these margins are wide (2% vs. 95% elapsed) so sub-second timing jitter
    // between capturing `now` and the component's own `Date.now()` tick can
    // never flip which bucket either row lands in.
    const countdownItems: InboxItem[] = [
      {
        id: 'approval:far',
        type: 'approval',
        title: 'Far from expiry',
        urgency: 20,
        actionable: true,
        timestamp: new Date(now - 10_000).toISOString(), // 2% of a 600s window
        expiresAt: nowSec + 590,
      },
      {
        id: 'approval:near',
        type: 'approval',
        title: 'Near expiry',
        urgency: 20,
        actionable: true,
        timestamp: new Date(now - 570_000).toISOString(), // 95% of a 600s window
        expiresAt: nowSec + 30,
      },
    ];
    renderWithProviders(
      <InboxList {...baseProps({ groups: [{ key: '', items: countdownItems }] })} />,
    );
    const tooltip = 'Left undecided past the deadline, this is auto-rejected';
    const farRow = screen.getByText('Far from expiry').closest('[role="option"]') as HTMLElement;
    const nearRow = screen.getByText('Near expiry').closest('[role="option"]') as HTMLElement;
    // Both rows render the countdown (identified by its explanatory tooltip,
    // not its exact numeric value — real-clock timing makes the latter
    // flaky to assert on).
    expect(within(farRow).getByTitle(tooltip)).toBeInTheDocument();
    expect(within(nearRow).getByTitle(tooltip)).toBeInTheDocument();
    // Only the near-expiry row's countdown carries the amber marker.
    expect(within(farRow).queryByText('Expiring soon')).not.toBeInTheDocument();
    expect(within(nearRow).getByText('Expiring soon')).toBeInTheDocument();
  });
});
