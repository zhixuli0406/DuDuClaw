import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { OrgNodePanel } from './OrgNodePanel';
import type { AgentDetail } from '@/lib/api';

const AGENT = {
  name: 'my-bot',
  display_name: 'My Bot',
  status: 'active',
  role: 'main',
  avatar: null,
} as unknown as AgentDetail;

describe('OrgNodePanel', () => {
  // X03 (UX audit §3.3): the panel's only exit used to be "查看員工詳情" into
  // AgentDetailPage — actually editing the staff member took 3 hops (node →
  // detail page → kebab menu → edit). `onEdit` adds a direct CrossLink.
  it('offers a direct edit CrossLink alongside the detail button', async () => {
    const user = userEvent.setup();
    const onEdit = vi.fn();
    renderWithProviders(
      <OrgNodePanel
        agent={AGENT}
        onOpenDetail={vi.fn()}
        onEdit={onEdit}
        onPause={vi.fn()}
        onResume={vi.fn()}
      />,
    );

    const link = screen.getByRole('button', { name: 'Edit this employee' });
    await user.click(link);

    expect(onEdit).toHaveBeenCalledTimes(1);
  });

  it('still exposes the existing detail button unaffected', () => {
    renderWithProviders(
      <OrgNodePanel
        agent={AGENT}
        onOpenDetail={vi.fn()}
        onEdit={vi.fn()}
        onPause={vi.fn()}
        onResume={vi.fn()}
      />,
    );
    expect(screen.getByRole('button', { name: /Open staff detail/ })).toBeInTheDocument();
  });
});
