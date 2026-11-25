import '@/test/mocks';
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { GoogleChatSharedSettings } from './GoogleChatSharedSettings';

describe('shared Google Chat settings', () => {
  it.each(['employee', 'manager', undefined])('hides company settings from %s', (role) => {
    renderWithProviders(<GoogleChatSharedSettings role={role} settings={{}} onChange={vi.fn()} />);
    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
  });

  it('edits the toggle and plain text and shows the literal URL placeholder', () => {
    const change = vi.fn();
    renderWithProviders(<GoogleChatSharedSettings role="admin" settings={{ shared_bot_binding: false, unbound_reply: 'Contact your administrator.' }} onChange={change} />);
    fireEvent.click(screen.getByRole('switch', { name: 'Route by sender (shared Chat app)' }));
    expect(change).toHaveBeenCalledWith('shared_bot_binding', true);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'Welcome {onboarding_url}' } });
    expect(change).toHaveBeenCalledWith('unbound_reply', 'Welcome {onboarding_url}');
    expect(screen.getByText(/Use \{onboarding_url\}/)).toBeInTheDocument();
  });
});
