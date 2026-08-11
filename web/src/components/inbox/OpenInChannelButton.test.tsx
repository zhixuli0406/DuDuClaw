import { describe, it, expect, vi } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { OpenInChannelButton } from './OpenInChannelButton';

describe('OpenInChannelButton', () => {
  it('renders nothing when there is no link (E8: never guess, never show a broken link)', () => {
    const { container } = renderWithProviders(<OpenInChannelButton channel="telegram" link={null} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders nothing when there is a link but no channel', () => {
    const { container } = renderWithProviders(<OpenInChannelButton channel={null} link="https://t.me/duduclaw_bot" />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders a button with the brand-named label when both are present', () => {
    renderWithProviders(<OpenInChannelButton channel="telegram" link="https://t.me/duduclaw_bot" />);
    expect(screen.getByRole('button', { name: /Open in Telegram/i })).toBeInTheDocument();
  });

  it('opens the link in a new tab on click', () => {
    const openSpy = vi.spyOn(window, 'open').mockImplementation(() => null);
    renderWithProviders(<OpenInChannelButton channel="discord" link="https://discord.com/channels/1/2/3" />);
    fireEvent.click(screen.getByRole('button', { name: /Open in Discord/i }));
    expect(openSpy).toHaveBeenCalledWith('https://discord.com/channels/1/2/3', '_blank', 'noopener,noreferrer');
    openSpy.mockRestore();
  });

  it('falls back to the raw channel key for an unmapped brand label', () => {
    renderWithProviders(<OpenInChannelButton channel="carrier_pigeon" link="https://example.com" />);
    expect(screen.getByRole('button', { name: /Open in carrier_pigeon/i })).toBeInTheDocument();
  });
});
