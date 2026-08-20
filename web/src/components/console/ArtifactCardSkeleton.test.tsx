import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { ArtifactCardSkeleton } from './ArtifactCardSkeleton';

describe('<ArtifactCardSkeleton>', () => {
  it('renders a default title when none is given', () => {
    renderWithProviders(<ArtifactCardSkeleton />);
    expect(screen.getByText('Preparing…')).toBeInTheDocument();
  });

  it('renders a caller-supplied label instead of the default', () => {
    renderWithProviders(<ArtifactCardSkeleton label="Checking device status…" />);
    expect(screen.getByText('Checking device status…')).toBeInTheDocument();
    expect(screen.queryByText('Preparing…')).not.toBeInTheDocument();
  });
});
