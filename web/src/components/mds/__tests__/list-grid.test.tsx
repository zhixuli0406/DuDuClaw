import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useLocation } from 'react-router';
import { renderWithProviders } from '@/test/render';
import {
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
} from '../list-grid';

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="pathname">{location.pathname}</div>;
}

describe('<ListGrid>', () => {
  it('fires the sort callback when a sortable header is clicked', async () => {
    const user = userEvent.setup();
    const onSort = vi.fn();
    renderWithProviders(
      <ListGridContainer
        columns="1fr 8rem"
        header={
          <ListGridHeader>
            <ListGridHeaderCell sortable sortDirection="asc" onSort={onSort}>
              Name
            </ListGridHeaderCell>
            <ListGridHeaderCell>Status</ListGridHeaderCell>
          </ListGridHeader>
        }
      />
    );
    const nameHeader = screen.getByRole('columnheader', { name: /Name/ });
    expect(nameHeader).toHaveAttribute('aria-sort', 'ascending');
    await user.click(screen.getByRole('button', { name: /Name/ }));
    expect(onSort).toHaveBeenCalledTimes(1);
  });

  it('navigates on a plain row click but yields to modifier keys', () => {
    const { container } = renderWithProviders(
      <>
        <ListGridContainer columns="1fr">
          <ListGridRow to="/target">
            <ListGridCell>Row content</ListGridCell>
          </ListGridRow>
        </ListGridContainer>
        <LocationProbe />
      </>
    );
    const row = container.querySelector('[data-slot="list-grid-row"]')!;

    // ⌘/ctrl-click must NOT hijack navigation (new-tab intent belongs to the link).
    fireEvent.click(row, { metaKey: true });
    expect(screen.getByTestId('pathname')).toHaveTextContent('/');

    // Plain primary click navigates.
    fireEvent.click(row);
    expect(screen.getByTestId('pathname')).toHaveTextContent('/target');
  });

  it('does not navigate when the click lands on an interactive descendant', () => {
    const { container } = renderWithProviders(
      <>
        <ListGridContainer columns="1fr">
          <ListGridRow to="/target">
            <ListGridCell>
              <button type="button">Kebab</button>
            </ListGridCell>
          </ListGridRow>
        </ListGridContainer>
        <LocationProbe />
      </>
    );
    fireEvent.click(container.querySelector('button')!);
    expect(screen.getByTestId('pathname')).toHaveTextContent('/');
  });

  it('applies the tall row size and shares the column template', () => {
    const { container } = renderWithProviders(
      <ListGridContainer columns="1fr 6rem">
        <ListGridRow rowSize="lg" selected>
          <ListGridCell>Name</ListGridCell>
          <ListGridCell hideBelow>Meta</ListGridCell>
        </ListGridRow>
      </ListGridContainer>
    );
    const row = container.querySelector('[data-slot="list-grid-row"]') as HTMLElement;
    expect(row).toHaveClass('min-h-16');
    expect(row).toHaveAttribute('data-selected', 'true');
    expect(row.style.gridTemplateColumns).toBe('1fr 6rem');
    const hidden = container.querySelectorAll('[data-slot="list-grid-cell"]')[1];
    expect(hidden).toHaveClass('hidden', '@2xl:flex');
  });
});
