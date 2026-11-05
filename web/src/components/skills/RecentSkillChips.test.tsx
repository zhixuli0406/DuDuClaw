import { useConnectionStore } from '@/stores/connection-store';
import { useState } from 'react';
import { act, fireEvent, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import '@/test/mocks';
import { api, type RecentSkill } from '@/lib/api';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { RecentSkillChips, toggleSkillToken } from './RecentSkillChips';

const skills = [
  { name: 'quote', display_name: 'Quote', description: 'Prepare a quote' },
  { name: 'pdf', display_name: 'PDF', description: 'Read a PDF' },
];

function Composer() {
  const [text, setText] = useState('');
  return <><RecentSkillChips value={text} onChange={setText} agentId="sales" />
    <input aria-label="Message" value={text} onChange={e => setText(e.target.value)} /></>;
}

beforeEach(() => {
  useConnectionStore.setState({ state: 'authenticated' });
  vi.restoreAllMocks();
  useAuthStore.setState({ user: { id: 'employee-a', role: 'employee' } as never });
});

describe('RecentSkillChips', () => {
  it('inserts multiple skills without submitting and reflects manual edits', async () => {
    const recent = vi.spyOn(api.skills, 'recent').mockResolvedValue({ skills });
    const submit = vi.fn();
    renderWithProviders(<form onSubmit={submit}><Composer /></form>);
    fireEvent.click(await screen.findByRole('button', { name: 'Quote Prepare a quote' }));
    fireEvent.click(screen.getByRole('button', { name: 'PDF Read a PDF' }));
    expect(screen.getByLabelText('Message')).toHaveValue('/quote /pdf ');
    expect(submit).not.toHaveBeenCalled();
    expect(recent).toHaveBeenCalledWith('sales', false);
    fireEvent.change(screen.getByLabelText('Message'), { target: { value: '/pdf details' } });
    expect(screen.getByRole('button', { name: 'Quote Prepare a quote' })).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByRole('button', { name: 'PDF Read a PDF' })).toHaveAttribute('aria-pressed', 'true');
  });

  it('clears the old account chips immediately and ignores late responses', async () => {
    let resolveOld!: (result: { skills: RecentSkill[] }) => void;
    const recent = vi.spyOn(api.skills, 'recent').mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve; }))
      .mockResolvedValue({ skills: [{ name: 'new', display_name: 'New account skill', description: '' }] });
    renderWithProviders(<Composer />);
    await waitFor(() => expect(recent).toHaveBeenCalledTimes(1));
    act(() => useAuthStore.setState({ user: { id: 'employee-b', role: 'employee' } as never }));
    expect(await screen.findByRole('button', { name: 'New account skill' })).toBeInTheDocument();
    await act(async () => resolveOld({ skills }));
    expect(screen.queryByRole('button', { name: 'Quote Prepare a quote' })).not.toBeInTheDocument();
  });

  it('shows at most five and safely omits an unavailable history', async () => {
    vi.spyOn(api.skills, 'recent').mockResolvedValue({ skills: Array.from({ length: 8 }, (_, n) => ({ name: `skill-${n}`, display_name: `Skill ${n}`, description: '' })) });
    const view = renderWithProviders(<Composer />);
    await screen.findByRole('button', { name: 'Skill 0' });
    expect(screen.getAllByRole('button')).toHaveLength(5);
    view.unmount();
    vi.spyOn(api.skills, 'recent').mockRejectedValue(new Error('unavailable'));
    renderWithProviders(<Composer />);
    await waitFor(() => expect(screen.queryAllByRole('button')).toHaveLength(0));
  });

  it('retries after reconnect and clears chips during a disconnect', async () => {
    const recent = vi.spyOn(api.skills, 'recent').mockRejectedValueOnce(new Error('disconnected'))
      .mockResolvedValue({ skills });
    renderWithProviders(<Composer />);
    await waitFor(() => expect(recent).toHaveBeenCalledTimes(1));
    act(() => useConnectionStore.setState({ state: 'disconnected' }));
    act(() => useConnectionStore.setState({ state: 'authenticated' }));
    expect(await screen.findByRole('button', { name: 'Quote Prepare a quote' })).toBeInTheDocument();
    act(() => useConnectionStore.setState({ state: 'disconnected' }));
    expect(screen.queryByRole('button', { name: 'Quote Prepare a quote' })).not.toBeInTheDocument();
  });

  it('removes only whole selected tokens and preserves similar names and prose', () => {
    expect(toggleSkillToken('/pdf /pdf-tools details', 'pdf')).toBe('/pdf-tools details');
    expect(toggleSkillToken('details/pdf', 'pdf')).toBe('details/pdf /pdf ');
  });
});
