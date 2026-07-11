import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { InboxList, type InboxGroup } from './InboxList';
import type { InboxRowLabels } from './InboxRow';
import type { InboxColumn, InboxItem } from '@/lib/inbox-model';

const items: InboxItem[] = [
  { id: 'approval:1', type: 'approval', title: 'Approve the deploy', urgency: 20, actionable: true, timestamp: '2026-01-01T00:00:00Z' },
  { id: 'blocked:2', type: 'blocked', title: 'Task is blocked', urgency: 30, actionable: true, timestamp: '2026-01-02T00:00:00Z' },
];

const labels: InboxRowLabels = {
  typeLabel: (i) => i.type,
  approve: 'Approve',
  reject: 'Reject',
  view: 'View',
  archive: 'Archive',
};

function baseProps(over?: Partial<React.ComponentProps<typeof InboxList>>) {
  return {
    // No label ⇒ no header row, so the keyboard cursor lands on a real row.
    groups: [{ key: '', items }] as InboxGroup[],
    columns: ['type', 'agent', 'time'] as InboxColumn[],
    canArchive: true,
    agentName: (id: string) => id,
    labels,
    emptyState: <div>empty</div>,
    onOpen: vi.fn(),
    onApprove: vi.fn(),
    onReject: vi.fn(),
    onView: vi.fn(),
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

  it('approve / view / archive actions fire their handlers', () => {
    const props = baseProps();
    renderWithProviders(<InboxList {...props} />);
    fireEvent.click(screen.getByText('Approve'));
    expect(props.onApprove).toHaveBeenCalled();
    fireEvent.click(screen.getByText('View'));
    expect(props.onView).toHaveBeenCalled();
    fireEvent.click(screen.getAllByText('Archive')[0]);
    expect(props.onArchive).toHaveBeenCalled();
  });

  it('clicking a row opens it', () => {
    const props = baseProps();
    renderWithProviders(<InboxList {...props} />);
    fireEvent.click(screen.getByText('Task is blocked'));
    expect(props.onOpen).toHaveBeenCalled();
  });

  it('keyboard: a archives, U marks unread, ⌘Z undoes', () => {
    const props = baseProps();
    renderWithProviders(<InboxList {...props} />);
    const listbox = screen.getByRole('listbox');
    fireEvent.keyDown(listbox, { key: 'a' });
    expect(props.onArchive).toHaveBeenCalled();
    fireEvent.keyDown(listbox, { key: 'U' });
    expect(props.onUnread).toHaveBeenCalled();
    fireEvent.keyDown(listbox, { key: 'z', metaKey: true });
    expect(props.onUndo).toHaveBeenCalled();
  });

  it('a does not archive when canArchive is false', () => {
    const props = baseProps({ canArchive: false });
    renderWithProviders(<InboxList {...props} />);
    const listbox = screen.getByRole('listbox');
    fireEvent.keyDown(listbox, { key: 'a' });
    expect(props.onArchive).not.toHaveBeenCalled();
  });
});
