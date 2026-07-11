import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { LiveBadge } from './LiveBadge';

describe('<LiveBadge>', () => {
  it('renders a default localized label', () => {
    renderWithProviders(<LiveBadge />);
    expect(screen.getByText('Live')).toBeInTheDocument();
  });

  it('renders custom trailing content', () => {
    renderWithProviders(<LiveBadge>3 runs</LiveBadge>);
    expect(screen.getByText('3 runs')).toBeInTheDocument();
  });
});
