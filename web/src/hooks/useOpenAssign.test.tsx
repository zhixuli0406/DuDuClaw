import { afterEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen } from '@testing-library/react';
import { useLocation } from 'react-router';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConversationsStore } from '@/stores/conversations-store';
import { useOpenAssign } from './useOpenAssign';
import type { AssignPrefill } from '@/lib/assign-entry';

function Entry({ prefill }: { prefill?: AssignPrefill }) {
  const open = useOpenAssign();
  const location = useLocation();
  return <>
    <button onClick={() => open(prefill)}>Open</button>
    <output data-testid="destination">{JSON.stringify({ path: location.pathname, state: location.state })}</output>
  </>;
}

afterEach(() => vi.restoreAllMocks());

describe('former assignment entry', () => {
  it('clears the existing conversation before opening chat', () => {
    const startNew = vi.spyOn(useConversationsStore.getState(), 'startNew').mockImplementation(() => {});
    renderWithProviders(<Entry />);
    fireEvent.click(screen.getByRole('button', { name: 'Open' }));
    expect(startNew).toHaveBeenCalledOnce();
    expect(JSON.parse(screen.getByTestId('destination').textContent!)).toEqual({ path: '/chat', state: null });
  });

  it('preserves description and criteria as a composer draft without selecting an agent or creating work', () => {
    const startNew = vi.spyOn(useConversationsStore.getState(), 'startNew').mockImplementation(() => {});
    renderWithProviders(<Entry prefill={{ description: '整理報價', acceptanceCriteria: '核對金額', agentId: 'main', mode: 'assign' }} />);
    fireEvent.click(screen.getByRole('button', { name: 'Open' }));
    expect(startNew).toHaveBeenCalledOnce();
    expect(JSON.parse(screen.getByTestId('destination').textContent!)).toEqual({
      path: '/chat', state: { draft: '整理報價\n\n核對金額' },
    });
  });
});
