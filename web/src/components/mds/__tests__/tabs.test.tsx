import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { Tabs, TabsList, TabsTab, TabsPanel } from '../tabs';

function Fixture({ variant }: { variant?: 'default' | 'line' }) {
  return (
    <Tabs defaultValue="a" variant={variant}>
      <TabsList>
        <TabsTab value="a">A</TabsTab>
        <TabsTab value="b">B</TabsTab>
      </TabsList>
      <TabsPanel value="a">Panel A</TabsPanel>
      <TabsPanel value="b">Panel B</TabsPanel>
    </Tabs>
  );
}

describe('<Tabs>', () => {
  it('renders the default filled track and switches panels on click', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Fixture />);
    const list = screen.getByRole('tablist');
    expect(list).toHaveClass('bg-muted', 'rounded-lg');

    expect(screen.getByText('Panel A')).toBeInTheDocument();
    const tabB = screen.getByRole('tab', { name: 'B' });
    await user.click(tabB);
    expect(tabB).toHaveAttribute('aria-selected', 'true');
    expect(await screen.findByText('Panel B')).toBeInTheDocument();
  });

  it('line variant drops the filled track background', () => {
    renderWithProviders(<Fixture variant="line" />);
    const list = screen.getByRole('tablist');
    expect(list).toHaveClass('bg-transparent');
    expect(list).not.toHaveClass('bg-muted');
  });

  // @base-ui/react 1.x marks the current tab with `data-active`, not
  // `data-selected`; selected styles keyed off the wrong attribute left the
  // current tab looking like every other tab.
  it.each(['default', 'line'] as const)('%s variant styles the tab base-ui marks active', (variant) => {
    renderWithProviders(<Fixture variant={variant} />);
    const active = screen.getByRole('tab', { name: 'A' });
    const inactive = screen.getByRole('tab', { name: 'B' });
    expect(active).toHaveAttribute('data-active');
    expect(inactive).not.toHaveAttribute('data-active');
    expect(active.className).toContain('data-[active]:text-foreground');
    expect(active.className).not.toContain('data-[selected]');
  });
});
