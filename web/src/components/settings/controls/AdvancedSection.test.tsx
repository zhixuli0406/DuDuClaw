import { describe, it, expect, beforeEach } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AdvancedSection } from './AdvancedSection';

describe('<AdvancedSection>', () => {
  beforeEach(() => localStorage.clear());

  it('hides children until toggled open', () => {
    renderWithProviders(
      <AdvancedSection storageKey="test.page" label="Advanced">
        <div>secret knob</div>
      </AdvancedSection>,
    );
    expect(screen.queryByText('secret knob')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Advanced' }));
    expect(screen.getByText('secret knob')).toBeInTheDocument();
  });

  it('remembers open state in localStorage per key', () => {
    const { unmount } = renderWithProviders(
      <AdvancedSection storageKey="mem.page" label="Advanced">
        <div>knob</div>
      </AdvancedSection>,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Advanced' }));
    expect(localStorage.getItem('dudu.settings.advanced.mem.page')).toBe('1');
    unmount();

    // Fresh mount reads persisted "open" state → children visible immediately.
    renderWithProviders(
      <AdvancedSection storageKey="mem.page" label="Advanced">
        <div>knob</div>
      </AdvancedSection>,
    );
    expect(screen.getByText('knob')).toBeInTheDocument();
  });
});
