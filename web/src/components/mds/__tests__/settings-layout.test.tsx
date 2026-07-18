import { describe, it, expect } from 'vitest';
import { useState } from 'react';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import {
  SettingsShell,
  SettingsTab,
  SettingsRow,
  SettingsCard,
  SettingsSaveState,
} from '../settings-layout';

describe('SettingsRow', () => {
  it.each([
    ['text', 'sm:w-96'],
    ['select-wide', 'sm:w-72'],
    ['select', 'sm:w-48'],
    ['code', 'sm:w-40'],
  ] as const)('sizes the control wrapper for tier "%s"', (tier, cls) => {
    const { container } = renderWithProviders(
      <SettingsRow label="Field" tier={tier}>
        <input />
      </SettingsRow>
    );
    const control = container.querySelector('[data-slot="settings-row-control"]')!;
    expect(control).toHaveClass(cls);
    expect(
      container.querySelector('[data-slot="settings-row"]')
    ).toHaveAttribute('data-tier', tier);
  });

  it('default tier applies no fixed width', () => {
    const { container } = renderWithProviders(
      <SettingsRow label="Field">
        <input />
      </SettingsRow>
    );
    const control = container.querySelector('[data-slot="settings-row-control"]')!;
    expect(control.className).not.toMatch(/sm:w-/);
  });
});

describe('SettingsSaveState', () => {
  it('renders nothing while idle', () => {
    const { container } = renderWithProviders(<SettingsSaveState status="idle" />);
    expect(container.querySelector('[data-slot="settings-save-state"]')).toBeNull();
  });

  it('shows a success indicator when saved', () => {
    renderWithProviders(<SettingsSaveState status="saved" />);
    const state = screen.getByRole('status');
    expect(state).toHaveAttribute('data-status', 'saved');
    expect(state).toHaveTextContent('Saved');
  });
});

describe('<SettingsShell>', () => {
  function Fixture() {
    const [tab, setTab] = useState('general');
    return (
      <SettingsShell
        value={tab}
        onValueChange={setTab}
        groups={[
          {
            label: 'Workspace',
            items: [
              { value: 'general', label: 'General' },
              { value: 'members', label: 'Members' },
            ],
          },
        ]}
      >
        <SettingsTab value="general" title="General">
          <SettingsCard>
            <SettingsRow label="Name">
              <input />
            </SettingsRow>
          </SettingsCard>
        </SettingsTab>
        <SettingsTab value="members" title="Members">
          Members pane
        </SettingsTab>
      </SettingsShell>
    );
  }

  it('switches the active pane through the rail', async () => {
    const user = userEvent.setup();
    renderWithProviders(<Fixture />);
    expect(screen.getByRole('heading', { name: 'General' })).toBeInTheDocument();
    await user.click(screen.getByRole('tab', { name: 'Members' }));
    expect(await screen.findByText('Members pane')).toBeInTheDocument();
  });

  it('divides the settings card rows', () => {
    const { container } = renderWithProviders(<Fixture />);
    const card = container.querySelector('[data-slot="settings-card"]')!;
    expect(card).toHaveClass('divide-y');
  });
});
