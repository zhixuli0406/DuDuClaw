import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { InboxIcon } from 'lucide-react';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '../sidebar';
import {
  CollectionPageHeader,
  CollectionPageState,
} from '../collection-page';

describe('<CollectionPageHeader>', () => {
  it('renders icon, title, count and description', () => {
    renderWithProviders(
      <SidebarProvider>
        <CollectionPageHeader
          icon={InboxIcon}
          title="Inbox"
          count={12}
          description="Unread items"
          action={<button type="button">New</button>}
        />
      </SidebarProvider>
    );
    expect(screen.getByRole('heading', { name: 'Inbox' })).toBeInTheDocument();
    expect(screen.getByText('12')).toHaveClass('font-mono', 'tabular-nums');
    expect(screen.getByText('Unread items')).toHaveClass('md:block');
    expect(screen.getByRole('button', { name: 'New' })).toBeInTheDocument();
  });

  it('renders separate compact and full actions when provided', () => {
    renderWithProviders(
      <SidebarProvider>
        <CollectionPageHeader
          title="Tasks"
          action={<button type="button">Full</button>}
          actionCompact={<button type="button">Icon</button>}
        />
      </SidebarProvider>
    );
    expect(screen.getByRole('button', { name: 'Full' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Icon' })).toBeInTheDocument();
  });

  it('omits the mobile sidebar trigger by default (shell provides it)', () => {
    renderWithProviders(
      <SidebarProvider>
        <CollectionPageHeader icon={InboxIcon} title="Inbox" />
      </SidebarProvider>
    );
    expect(
      screen.queryByRole('button', { name: 'Toggle sidebar' })
    ).not.toBeInTheDocument();
  });

  it('renders the trigger when hideTrigger is explicitly false', () => {
    renderWithProviders(
      <SidebarProvider>
        <CollectionPageHeader hideTrigger={false} icon={InboxIcon} title="Inbox" />
      </SidebarProvider>
    );
    expect(
      screen.getByRole('button', { name: 'Toggle sidebar' })
    ).toBeInTheDocument();
  });
});

describe('<CollectionPageState>', () => {
  it('renders a skeleton stack while loading', () => {
    const { container } = renderWithProviders(
      <CollectionPageState state="loading" />
    );
    expect(
      container.querySelectorAll('[data-slot="collection-page-skeleton"]').length
    ).toBe(5);
  });

  it('renders an empty state', () => {
    renderWithProviders(
      <CollectionPageState state="empty" title="No tasks" icon={InboxIcon} />
    );
    expect(screen.getByText('No tasks')).toBeInTheDocument();
  });

  it('renders a destructive error state', () => {
    const { container } = renderWithProviders(
      <CollectionPageState state="error" title="Failed" />
    );
    const empty = container.querySelector('[data-slot="empty"]')!;
    expect(empty).toHaveAttribute('data-tone', 'destructive');
  });
});
