import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  CardFooter,
} from '../card';

describe('<Card>', () => {
  it('renders the full card composition', () => {
    renderWithProviders(
      <Card data-testid="card">
        <CardHeader>
          <CardTitle>Title</CardTitle>
          <CardDescription>Desc</CardDescription>
        </CardHeader>
        <CardContent>Body</CardContent>
        <CardFooter>Foot</CardFooter>
      </Card>
    );
    const card = screen.getByTestId('card');
    expect(card).toHaveClass('rounded-xl', 'bg-surface', 'shadow-[var(--surface-shadow)]');
    expect(screen.getByText('Title')).toHaveClass('text-base', 'font-medium');
    expect(screen.getByText('Desc')).toHaveClass('text-muted-foreground');
    expect(screen.getByText('Foot')).toHaveClass('border-t');
  });

  it('tightens spacing with data-size=sm', () => {
    renderWithProviders(
      <Card data-size="sm" data-testid="card">
        x
      </Card>
    );
    expect(screen.getByTestId('card')).toHaveClass('data-[size=sm]:py-3');
    expect(screen.getByTestId('card')).toHaveAttribute('data-size', 'sm');
  });
});
