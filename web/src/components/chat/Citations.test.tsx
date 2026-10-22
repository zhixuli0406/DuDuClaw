import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, fireEvent } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { MessageBubble } from './MessageBubble';
import type { ChatMessage } from '@/stores/chat-store';
import type { ChatCitation, KnowledgePassage } from '@/lib/knowledge-citations';

const passageGet = vi.fn();
// Rejections go through a plain function so vi.fn's result tracking does not
// create an unhandled derived promise.
const failNext: { err: Error | null; once: boolean } = { err: null, once: false };
async function failWith() {
  const e = failNext.err!;
  if (failNext.once) failNext.err = null;
  throw e;
}
vi.mock('@/lib/ws-client', () => ({
  client: { call: (m: string, p: unknown) => (m === 'knowledge.passage.get' ? (failNext.err ? failWith() : passageGet(p)) : Promise.reject(new Error('x'))) },
}));

const cite = (o: Partial<ChatCitation> & { n: number }): ChatCitation => ({
  chunk_id: o.n * 10, source_id: 's', title: '員工手冊.pdf', locator: '第 3 頁', excerpt: '特休說明', state: 'available', ...o,
});
const passage = (o: Partial<KnowledgePassage> = {}): KnowledgePassage => ({
  chunk_id: 10, source_id: 's', collection_id: 'c', title: '員工手冊.pdf', locator_text: '第 3 頁',
  heading_path: ['福利'], segment_label: '段落 2', before: 'BEFORE ', match: '<b>MATCH</b>', after: ' AFTER',
  prev_chunk_id: 9, next_chunk_id: null, suspect: false, ...o,
});
const msg = (content: string, citations: ChatCitation[]): ChatMessage => ({
  id: 'm1', role: 'assistant', content, timestamp: 0, citations,
});

beforeEach(() => {
  passageGet.mockReset();
  failNext.err = null;
});

describe('citation rendering', () => {
  it('renders valid markers as buttons and leaves unknown ones plain', () => {
    renderWithProviders(<MessageBubble message={msg('特休 [1] 與 [5]', [cite({ n: 1 })])} />);
    expect(screen.getByRole('button', { name: /Source 1: 員工手冊\.pdf 第 3 頁/ })).toBeTruthy();
    expect(document.body.textContent).toContain('[5]');
    expect(document.querySelectorAll('[data-citation-marker]')).toHaveLength(1);
  });

  it('does not turn markers inside code into buttons', () => {
    renderWithProviders(<MessageBubble message={msg('`a[1]`', [cite({ n: 1 })])} />);
    expect(document.querySelectorAll('[data-citation-marker]')).toHaveLength(0);
  });

  it('deleted and restricted rows are not clickable', () => {
    renderWithProviders(
      <MessageBubble
        message={msg('x [1] [2]', [
          cite({ n: 1, state: 'deleted', chunk_id: null, excerpt: '' }),
          cite({ n: 2, state: 'restricted', chunk_id: null, title: '', locator: '', excerpt: '' }),
        ])}
      />,
    );
    expect(screen.getByText('Source deleted')).toBeTruthy();
    expect(screen.getByText('You do not have permission to view this source')).toBeTruthy();
    const rows = document.querySelectorAll('[data-citation-row]');
    expect(rows).toHaveLength(2);
    rows.forEach((r) => expect(r.querySelector('button')).toBeNull());
    document.querySelectorAll<HTMLButtonElement>('[data-citation-marker]').forEach((b) => expect(b.disabled).toBe(true));
  });

  it('opens the drawer, shows before/match/after as text and navigates', async () => {
    passageGet.mockResolvedValueOnce(passage()).mockResolvedValueOnce(passage({ chunk_id: 9, prev_chunk_id: null, match: 'PREV' }));
    renderWithProviders(<MessageBubble message={msg('x [1]', [cite({ n: 1 })])} />);
    fireEvent.click(screen.getByRole('button', { name: /Source 1/ }));
    const dlg = await screen.findByRole('dialog');
    expect(dlg.getAttribute('aria-modal')).toBe('true');
    await waitFor(() => expect(dlg.querySelector('mark')?.textContent).toBe('<b>MATCH</b>'));
    expect(dlg.querySelector('mark b')).toBeNull();
    expect(dlg.textContent).toContain('BEFORE');
    expect(dlg.textContent).toContain('AFTER');
    expect(passageGet).toHaveBeenCalledWith({ chunk_id: 10 });
    expect((screen.getByText('Next passage').closest('button') as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByText('Previous passage'));
    await waitFor(() => expect(dlg.querySelector('mark')?.textContent).toBe('PREV'));
    expect(passageGet).toHaveBeenLastCalledWith({ chunk_id: 9 });
  });

  it('shows a friendly message on permission denied and closes on Escape', async () => {
    failNext.err = new Error('permission denied');
    renderWithProviders(<MessageBubble message={msg('x [1]', [cite({ n: 1 })])} />);
    const trigger = screen.getByRole('button', { name: /Source 1/ });
    trigger.focus();
    fireEvent.click(trigger);
    expect(await screen.findByText('You do not have permission to view this passage')).toBeTruthy();
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(trigger);
  });

  it('shows a generic error and the suspect notice', async () => {
    failNext.err = new Error('boom');
    failNext.once = true;
    const { unmount } = renderWithProviders(<MessageBubble message={msg('x [1]', [cite({ n: 1 })])} />);
    fireEvent.click(screen.getByRole('button', { name: /Source 1/ }));
    expect(await screen.findByText(/Could not load this passage/)).toBeTruthy();
    unmount();
    passageGet.mockResolvedValueOnce(passage({ suspect: true }));
    renderWithProviders(<MessageBubble message={msg('x [1]', [cite({ n: 1 })])} />);
    fireEvent.click(screen.getByRole('button', { name: /Source 1/ }));
    expect(await screen.findByText(/may contain instructions/)).toBeTruthy();
  });
});
