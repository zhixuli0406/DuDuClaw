import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '../table';

describe('<Table>', () => {
  it('renders a table with header + rows and expected classes', () => {
    renderWithProviders(
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          <TableRow data-state="selected">
            <TableCell>Alice</TableCell>
          </TableRow>
        </TableBody>
      </Table>
    );
    expect(screen.getByRole('table')).toHaveClass('w-full', 'text-sm');
    expect(screen.getByText('Name')).toHaveClass('h-10', 'font-medium');
    const row = screen.getByText('Alice').closest('tr')!;
    expect(row).toHaveClass('hover:bg-muted/50', 'data-[state=selected]:bg-muted');
    expect(row).toHaveAttribute('data-state', 'selected');
  });
});
