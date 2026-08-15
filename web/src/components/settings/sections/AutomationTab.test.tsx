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

  it('reflects a saved evaluator_only mode from system.config', async () => {
    configMock.mockResolvedValue(configWithJudge('evaluator_only'));
    renderWithProviders(<AutomationTab />);

    const trigger = await screen.findByRole('combobox', { name: 'Acceptance judge' });
    await waitFor(() =>
      expect(trigger).toHaveTextContent(
        'Quick mode: a lighter first-pass check only, review is more lenient — good for low-risk routine tasks'
      )
    );
  });

  it('lists all four judge modes and lets the user switch between them', async () => {
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
        name: 'Quick mode: a lighter first-pass check only, review is more lenient — good for low-risk routine tasks',
      })
    ).toBeInTheDocument();
    expect(
      screen.getByRole('option', {
        name: 'External judge: hand the decision to your own program, configured in config.toml',
      })
    ).toBeInTheDocument();
    expect(
      screen.getByRole('option', {
        name: 'Always human review: every finished item waits for your personal confirmation',
      })
    ).toBeInTheDocument();

    await user.click(
      screen.getByRole('option', {
        name: 'Always human review: every finished item waits for your personal confirmation',
      })
    );
    await waitFor(() =>
      expect(trigger).toHaveTextContent(
        'Always human review: every finished item waits for your personal confirmation'
      )
    );
  });

  it('shows a risk callout only in quick (evaluator_only) mode', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AutomationTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    await screen.findByRole('combobox', { name: 'Acceptance judge' });

    // Default (mav): no risk callout, no external-command hint.
    expect(screen.queryByText(/Risk: Quick mode/)).not.toBeInTheDocument();
    expect(screen.queryByText(/judge_command/)).not.toBeInTheDocument();

    await user.click(screen.getByRole('combobox', { name: 'Acceptance judge' }));
    await screen.findByRole('listbox');
    await user.click(
      screen.getByRole('option', {
        name: 'Quick mode: a lighter first-pass check only, review is more lenient — good for low-risk routine tasks',
      })
    );
    expect(await screen.findByText(/Risk: Quick mode/)).toBeInTheDocument();
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
