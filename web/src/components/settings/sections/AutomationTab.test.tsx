import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';

// AutomationTab reads the whole `config.toml` (masked) via system.config on
// mount and writes every field back in one system.update_config payload on
// save. This file exercises only the WP-6B acceptance-judge row
// (`[dispatch] judge` — see judge_mode.rs on the gateway side); the other
// fields on this tab already have their own coverage in the change history.
const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (fields: Record<string, unknown>) => updateConfigMock(fields),
    },
  },
}));

import { AutomationTab } from './AutomationTab';

// Minimal but complete TOML so every field this component reads has a
// section to parse (absent sections just fall back to compiled-in defaults,
// but keeping them explicit here makes the fixture self-documenting).
function configWithJudge(judge: string): { config: string } {
  return {
    config: `[goal_loop]
planner_enabled = false
iteration_cap_simple = 3
resume_on_restart = "pause"

[dispatch]
enabled = true
policy = "fixed_hierarchy"
judge = "${judge}"

[topology_evolution]
enabled = false

[knowledge_guard]
enabled = true
window_secs = 3600
max_per_subject = 5

[memory]
graph_embed_seed = false

[belief]
flat_band_pct = 0.3
`,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  configMock.mockResolvedValue(configWithJudge('mav'));
  updateConfigMock.mockResolvedValue({ success: true, hot_reloaded: [] });
});

describe('<AutomationTab> acceptance judge (dispatch.judge, WP-6B)', () => {
  it('shows the standard-review option as the current value when config.toml has no override', async () => {
    renderWithProviders(<AutomationTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());

    const trigger = await screen.findByRole('combobox', { name: 'Acceptance judge' });
    expect(trigger).toHaveTextContent(
      'Standard review (default): a three-aspect AI judge panel checks the work'
    );
  });

  // v1.69.0 removed `evaluator_only` / `human_only`. A config.toml that still
  // holds one must show that value (labelled with what actually runs now),
  // never silently display the default.
  it('shows a saved human_only as removed, with what happens now, instead of the default', async () => {
    configMock.mockResolvedValue(configWithJudge('human_only'));
    renderWithProviders(<AutomationTab />);

    const trigger = await screen.findByRole('combobox', { name: 'Acceptance judge' });
    await waitFor(() => expect(trigger).toHaveTextContent(/Always human review \(removed\)/));
    expect(trigger).not.toHaveTextContent(/Standard review/);
    expect(trigger).toHaveTextContent(/stops at "Needs your decision"/);
    const notice = await screen.findByText(/removed in v1\.69\.0/);
    expect(notice).toHaveTextContent(/Autonomy level/);
    expect(notice).toHaveTextContent(/stops at "Needs your decision"/);
  });

  it('shows a saved evaluator_only (or its alias) as removed and running as standard review', async () => {
    for (const raw of ['evaluator_only', 'Evaluator']) {
      configMock.mockResolvedValue(configWithJudge(raw));
      const { unmount } = renderWithProviders(<AutomationTab />);
      const trigger = await screen.findByRole('combobox', { name: 'Acceptance judge' });
      await waitFor(() => expect(trigger).toHaveTextContent(/Quick mode \(removed\)/));
      expect(trigger).toHaveTextContent(/runs as standard review/);
      expect(await screen.findByText(/removed in v1\.69\.0/)).toHaveTextContent(/stricter/);
      unmount();
    }
  });

  it('offers only the two supported judge modes and lets the user switch between them', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AutomationTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());

    const trigger = await screen.findByRole('combobox', { name: 'Acceptance judge' });
    await user.click(trigger);
    await screen.findByRole('listbox');

    expect(
      screen.getByRole('option', {
        name: 'Standard review (default): a three-aspect AI judge panel checks the work',
      })
    ).toBeInTheDocument();
    expect(
      screen.getByRole('option', {
        name: 'External judge: hand the decision to your own program, configured in config.toml',
      })
    ).toBeInTheDocument();
    // The two removed modes are not offered.
    expect(screen.queryByRole('option', { name: /Quick mode/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('option', { name: /Always human review/ })).not.toBeInTheDocument();

    await user.click(
      screen.getByRole('option', {
        name: 'External judge: hand the decision to your own program, configured in config.toml',
      })
    );
    await waitFor(() =>
      expect(trigger).toHaveTextContent(
        'External judge: hand the decision to your own program, configured in config.toml'
      )
    );
  });

  it('lists only the saved removed value, and moving off it drops it from the list', async () => {
    const user = userEvent.setup();
    configMock.mockResolvedValue(configWithJudge('human_only'));
    renderWithProviders(<AutomationTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());

    const trigger = await screen.findByRole('combobox', { name: 'Acceptance judge' });
    await user.click(trigger);
    await screen.findByRole('listbox');
    expect(screen.getByRole('option', { name: /Always human review \(removed\)/ })).toBeInTheDocument();
    // …and the other removed mode, which is NOT the saved value, stays out.
    expect(screen.queryByRole('option', { name: /Quick mode/ })).not.toBeInTheDocument();

    await user.click(
      screen.getByRole('option', {
        name: 'Standard review (default): a three-aspect AI judge panel checks the work',
      })
    );
    await waitFor(() => expect(screen.queryByText(/removed in v1\.69\.0/)).not.toBeInTheDocument());
    await user.click(trigger);
    await screen.findByRole('listbox');
    expect(screen.queryByRole('option', { name: /Always human review/ })).not.toBeInTheDocument();
  });

  it('switching a saved removed value to standard review sends mav; leaving it sends nothing', async () => {
    const user = userEvent.setup();
    configMock.mockResolvedValue(configWithJudge('human_only'));
    renderWithProviders(<AutomationTab />);
    await screen.findByRole('combobox', { name: 'Acceptance judge' });

    // Untouched: the removed value is never written back.
    await user.click(screen.getByRole('button', { name: /^Save$/i }));
    expect(updateConfigMock).not.toHaveBeenCalled();

    await user.click(screen.getByRole('combobox', { name: 'Acceptance judge' }));
    await screen.findByRole('listbox');
    await user.click(
      screen.getByRole('option', {
        name: 'Standard review (default): a three-aspect AI judge panel checks the work',
      })
    );
    await user.click(screen.getByRole('button', { name: /^Save$/i }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    const payload = updateConfigMock.mock.calls[0][0] as { dispatch: { judge: string } };
    expect(payload.dispatch.judge).toBe('mav');
  });

  it('shows no removal notice and no external hint in the default mode', async () => {
    renderWithProviders(<AutomationTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    await screen.findByRole('combobox', { name: 'Acceptance judge' });
    expect(screen.queryByText(/removed in v1\.69\.0/)).not.toBeInTheDocument();
    expect(screen.queryByText(/judge_command/)).not.toBeInTheDocument();
  });

  it('shows the config.toml judge_command hint only in external mode', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AutomationTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    await screen.findByRole('combobox', { name: 'Acceptance judge' });

    await user.click(screen.getByRole('combobox', { name: 'Acceptance judge' }));
    await screen.findByRole('listbox');
    await user.click(
      screen.getByRole('option', {
        name: 'External judge: hand the decision to your own program, configured in config.toml',
      })
    );
    expect(await screen.findByText(/judge_command/)).toBeInTheDocument();
    // The dashboard deliberately offers no input for the command itself —
    // just the one hint paragraph, no text field labeled for it.
    expect(screen.queryByLabelText(/judge_command/i)).not.toBeInTheDocument();
  });

  it('sends the selected judge mode inside the dispatch payload on save', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AutomationTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());

    await user.click(await screen.findByRole('combobox', { name: 'Acceptance judge' }));
    await screen.findByRole('listbox');
    await user.click(
      screen.getByRole('option', {
        name: 'External judge: hand the decision to your own program, configured in config.toml',
      })
    );

    await user.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    const payload = updateConfigMock.mock.calls[0][0] as { dispatch: { judge: string } };
    expect(payload.dispatch.judge).toBe('external');
    // judge_command / judge_timeout_secs must never be sent from the
    // dashboard — the gateway RPC does not even accept them (judge_mode.rs).
    expect(payload.dispatch).not.toHaveProperty('judge_command');
    expect(payload.dispatch).not.toHaveProperty('judge_timeout_secs');
  });
});
