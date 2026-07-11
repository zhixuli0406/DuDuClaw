import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { PropertySection, PropertyRow } from './PropertyRow';

describe('<PropertySection> / <PropertyRow>', () => {
  it('renders a labeled section with rows', () => {
    renderWithProviders(
      <PropertySection title="分流">
        <PropertyRow label="狀態">進行中</PropertyRow>
      </PropertySection>,
    );
    expect(screen.getByText('分流')).toBeInTheDocument();
    expect(screen.getByText('狀態')).toBeInTheDocument();
    expect(screen.getByText('進行中')).toBeInTheDocument();
  });

  it('an interactive row is a button', () => {
    const onClick = vi.fn();
    renderWithProviders(
      <PropertyRow label="指派" onClick={onClick}>
        小助手
      </PropertyRow>,
    );
    fireEvent.click(screen.getByRole('button'));
    expect(onClick).toHaveBeenCalled();
  });
});
