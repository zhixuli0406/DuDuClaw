import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
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
});
