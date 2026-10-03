import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (fields: Record<string, unknown>) => updateConfigMock(fields),
    },
    tickSources: { list: () => Promise.resolve({ sources: [] }) },
  },
}));

import { AutomationTab } from './AutomationTab';

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [] as never });
  configMock.mockResolvedValue({
    config: `[dispatch]
enabled = true
policy = "fixed_hierarchy"
judge = "mav"
judge_provider = "codex"

[night]
llm_enabled = false
`,
  });
  updateConfigMock.mockResolvedValue({ success: true, hot_reloaded: [], restart_required: [] });
});

function mainSave() {
  // The tab's original Save is the first plain "Save" button.
  return screen.getAllByRole('button', { name: /^Save$/ })[0];
}

describe('<AutomationTab> v1.68 rows', () => {
  it('reads the judge runtime and sends only the night switch when that is all that changed', async () => {
    renderWithProviders(<AutomationTab />);
    await waitFor(() =>
      expect(screen.getByRole('combobox', { name: 'Runtime for the acceptance judge' })).toHaveTextContent('Codex'),
    );
    fireEvent.click(screen.getByRole('switch', { name: 'Night tidy-up (model stage)' }));
    fireEvent.click(mainSave());
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ night: { llm_enabled: true } });
  });

  it('offers role_team and sends judge model + policy as nested dispatch keys', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AutomationTab />);
    await waitFor(() =>
      expect(screen.getByRole('combobox', { name: 'Runtime for the acceptance judge' })).toHaveTextContent('Codex'),
    );
    await user.click(screen.getByRole('combobox', { name: 'Dispatch policy' }));
    await user.click(await screen.findByRole('option', { name: /One employee in four roles/ }));
    fireEvent.change(screen.getByLabelText('Model for the acceptance judge'), { target: { value: 'gpt-5-codex' } });
    fireEvent.click(mainSave());
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({
      dispatch: { policy: 'role_team', judge_model: 'gpt-5-codex' },
    });
  });
});
