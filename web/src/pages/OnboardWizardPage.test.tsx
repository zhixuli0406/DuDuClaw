import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

vi.mock('@/lib/api', () => ({
  api: { agents: { create: vi.fn() } },
}));

import { OnboardWizardPage } from './OnboardWizardPage';

/**
 * WP5.1 — OnboardWizardPage Multica migration smoke test. Locks in the app-shell
 * surface, the wizard title, and the step-1 industry picker + Next control.
 */
describe('<OnboardWizardPage>', () => {
  it('renders the wizard header, indicator and the industry step', () => {
    renderWithProviders(<OnboardWizardPage />);

    expect(screen.getByRole('heading', { name: 'Setup Wizard' })).toBeInTheDocument();
    // Step-1 industry cards + the Next affordance (disabled until a pick).
    expect(screen.getByRole('button', { name: /Restaurant/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Next' })).toBeInTheDocument();
  });
});
