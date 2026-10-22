import { describe, it, expect, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';

vi.mock('@/lib/api', () => ({
  api: {
    experts: {
      generate: vi.fn(),
      generateRevise: vi.fn(),
      generateStatus: vi.fn(),
      generateCancel: vi.fn(() => Promise.resolve({ cancelled: true, state: 'cancelled' })),
      installDraft: vi.fn(),
    },
  },
}));
vi.mock('@/lib/ws-client', () => ({
  client: { subscribe: () => () => {}, call: vi.fn() },
}));
vi.mock('./AttachUnderSelect', () => ({ AttachUnderSelect: () => null }));
vi.mock('@/components/chat/ThinkingOrbIndicator', () => ({ ThinkingOrbIndicator: () => null }));

import { GenerateExpertDialog } from './GenerateExpertDialog';
import { api, type ExpertDraftStatus } from '@/lib/api';

/** Review of PR #166 (M2): cancelling while `experts.generate` is still in
 *  flight must stop the job the gateway is about to start, not orphan it. */
describe('<GenerateExpertDialog> cancel before the job id arrives', () => {
  it('cancels the started job once its id is known', async () => {
    const user = userEvent.setup();
    let resolveStart: (v: ExpertDraftStatus) => void = () => {};
    vi.mocked(api.experts.generate).mockReturnValue(
      new Promise<ExpertDraftStatus>((r) => {
        resolveStart = r;
      }),
    );
    renderWithProviders(
      <GenerateExpertDialog open onOpenChange={vi.fn()} onInstalled={vi.fn()} />,
    );
    await user.type(screen.getByLabelText(en['experts.generate.description']), 'shop support');
    await user.click(screen.getByRole('button', { name: en['experts.generate.start'] }));
    await user.click(await screen.findByRole('button', { name: en['common.cancel'] }));
    resolveStart({ draft_id: 'late-draft', status: 'running', job: null });
    await waitFor(() => expect(api.experts.generateCancel).toHaveBeenCalledWith('late-draft'));
    expect(api.experts.generateStatus).not.toHaveBeenCalled();
  });
});
