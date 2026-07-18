import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '../sidebar';
import { BreadcrumbHeader } from '../breadcrumb-header';

describe('<BreadcrumbHeader>', () => {
  it('joins segments with separators and truncates the leaf', () => {
    const { container } = renderWithProviders(
      <SidebarProvider>
        <BreadcrumbHeader
          segments={[
            { label: 'Tasks', href: '/tasks' },
            { label: 'Ship the redesign' },
          ]}
          actions={<button type="button">Done</button>}
        />
      </SidebarProvider>
    );
    // one ChevronRight separator between two segments
    expect(
      container.querySelectorAll('nav svg').length
    ).toBe(1);
    expect(screen.getByText('Ship the redesign')).toHaveClass(
      'max-w-72',
      'font-medium'
    );
    expect(screen.getByRole('button', { name: 'Done' })).toBeInTheDocument();
  });

  it('invokes a segment onClick handler', async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    renderWithProviders(
      <SidebarProvider>
        <BreadcrumbHeader
          segments={[
            { label: 'Root', onClick },
            { label: 'Leaf' },
          ]}
        />
      </SidebarProvider>
    );
    await user.click(screen.getByText('Root'));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it('omits the mobile sidebar trigger by default (shell provides it)', () => {
    renderWithProviders(
      <SidebarProvider>
        <BreadcrumbHeader segments={[{ label: 'Root' }, { label: 'Leaf' }]} />
      </SidebarProvider>
    );
    expect(
      screen.queryByRole('button', { name: 'Toggle sidebar' })
    ).not.toBeInTheDocument();
  });

  it('renders the trigger when hideTrigger is explicitly false', () => {
    renderWithProviders(
      <SidebarProvider>
        <BreadcrumbHeader
          hideTrigger={false}
          segments={[{ label: 'Root' }, { label: 'Leaf' }]}
        />
      </SidebarProvider>
    );
    expect(
      screen.getByRole('button', { name: 'Toggle sidebar' })
    ).toBeInTheDocument();
  });
});
