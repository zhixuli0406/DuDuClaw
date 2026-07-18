import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '../sidebar';
import { PageHeader } from '../page-header';

describe('<PageHeader>', () => {
  it('renders children and an md:hidden sidebar trigger', () => {
    const { container } = renderWithProviders(
      <SidebarProvider>
        <PageHeader>
          <span>Title</span>
        </PageHeader>
      </SidebarProvider>
    );
    expect(screen.getByText('Title')).toBeInTheDocument();
    const trigger = screen.getByRole('button', { name: 'Toggle sidebar' });
    expect(trigger).toHaveClass('md:hidden');
    expect(container.querySelector('[data-slot="page-header"]')).toHaveClass(
      'h-12',
      'border-b'
    );
  });

  it('omits the trigger when hideTrigger is set', () => {
    renderWithProviders(
      <SidebarProvider>
        <PageHeader hideTrigger>
          <span>Title</span>
        </PageHeader>
      </SidebarProvider>
    );
    expect(
      screen.queryByRole('button', { name: 'Toggle sidebar' })
    ).not.toBeInTheDocument();
  });
});
