import { describe, it, expect, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import {
  SidebarProvider,
  Sidebar,
  SidebarInset,
  SidebarTrigger,
  SidebarMenu,
  SidebarMenuItem,
  SidebarMenuButton,
  SidebarMenuBadge,
} from '../sidebar';

function Shell() {
  return (
    <SidebarProvider>
      <Sidebar>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton isActive>
              Home <SidebarMenuBadge>3</SidebarMenuBadge>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </Sidebar>
      <SidebarInset>
        <SidebarTrigger />
      </SidebarInset>
    </SidebarProvider>
  );
}

describe('<Sidebar>', () => {
  beforeEach(() => localStorage.clear());

  it('toggles between expanded and collapsed (3rem icon mode)', async () => {
    const user = userEvent.setup();
    const { container } = renderWithProviders(<Shell />);
    const aside = container.querySelector('[data-slot="sidebar"]')!;

    expect(aside).toHaveAttribute('data-state', 'expanded');
    expect((aside as HTMLElement).style.width).toBe('256px');

    await user.click(screen.getByRole('button', { name: 'Toggle sidebar' }));
    expect(aside).toHaveAttribute('data-state', 'collapsed');
    expect((aside as HTMLElement).style.width).toBe('3rem');
  });

  it('persists the open state to localStorage', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Shell />);
    await user.click(screen.getByRole('button', { name: 'Toggle sidebar' }));
    expect(localStorage.getItem('sidebar_open')).toBe('0');
  });

  it('marks the active menu button and renders a badge', () => {
    renderWithProviders(<Shell />);
    const btn = screen.getByRole('button', { name: /Home/ });
    expect(btn).toHaveAttribute('data-active', 'true');
    expect(btn).toHaveClass('data-[active=true]:bg-sidebar-accent');
    expect(screen.getByText('3')).toHaveClass('ml-auto');
  });

  it('renders SidebarInset as the page-canvas surface', () => {
    const { container } = renderWithProviders(<Shell />);
    const inset = container.querySelector('[data-slot="sidebar-inset"]')!;
    expect(inset).toHaveClass('bg-page-canvas', 'rounded-xl', 'ring-1');
  });
});
