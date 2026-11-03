import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import { useLocation } from 'react-router';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { MobileBottomNav } from './MobileBottomNav';

function Location() {
  return <output data-testid="location">{useLocation().pathname}</output>;
}
afterEach(() => vi.restoreAllMocks());

describe('mobile new conversation', () => {
  it.each(['admin', 'manager', 'employee'] as const)('hides task navigation and resets chat for %s', role => {
    useAuthStore.setState({ user: { id: 'u1', role } as never });
    useSystemStore.setState({ status: { edition_profile: 'enterprise', employee_tasks: true } as never });
    const startNew = vi.spyOn(useConversationsStore.getState(), 'startNew').mockImplementation(() => {});
    const { container } = renderWithProviders(<><MobileBottomNav /><Location /></>);
    expect(container.querySelector('a[href="/tasks"], a[href="/goals"], a[href="/assign"]')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'New chat' }));
    expect(startNew).toHaveBeenCalledOnce();
    expect(screen.getByTestId('location')).toHaveTextContent('/chat');
  });
});
