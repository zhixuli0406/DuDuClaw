import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { SearchPreviewTab, snippet } from './SearchPreviewTab';
import { whereText } from '@/lib/knowledge-api';
import { callsOf, collection, mockRpc, renderAt } from './test-fixtures';

const HIT = {
  chunk_id: 11,
  source_id: 'src_1',
  collection_id: 'col_1',
  title: 'leave.pdf',
  locator_text: 'page 2',
  heading_path: 'Chapter 2 > Leave',
  text: 'Annual leave is 14 days for staff with one year of service.',
  score: 0.9,
  suspect: false,
};

beforeEach(() => vi.clearAllMocks());

describe('SearchPreviewTab', () => {
  it('searches only this collection and lists ranked passages with where they sit', async () => {
    const user = userEvent.setup();
    mockRpc({
      'knowledge.search': () => ({ hits: [HIT, { ...HIT, chunk_id: 12, title: 'faq.md', locator_text: '', heading_path: '', suspect: true }], mode: 'keyword_only', vector_unavailable: 'off', scan_capped: false }),
      'knowledge.passage.get': () => ({ ...HIT, segment_label: null, before: '', match: HIT.text, after: '', prev_chunk_id: null, next_chunk_id: null }),
    });
    renderAt(<SearchPreviewTab collection={collection()} />);
    await user.type(screen.getByRole('textbox'), '特休幾天');
    await user.keyboard('{Enter}');
    expect(await screen.findByText('[1]')).toBeInTheDocument();
    expect(callsOf('knowledge.search')[0]).toEqual({ query: '特休幾天', limit: 10, collections: ['col_1'] });
    expect(screen.getByText('page 2 · Chapter 2 > Leave')).toBeInTheDocument();
    expect(screen.getByText('Word match')).toBeInTheDocument();
    expect(screen.getByText('This search matched words only.')).toBeInTheDocument();
    expect(screen.getByText('Looks like instructions')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /leave\.pdf/ }));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    await waitFor(() => expect(callsOf('knowledge.passage.get')).toEqual([{ chunk_id: 11 }]));
  });

  it('says so when nothing matches', async () => {
    const user = userEvent.setup();
    mockRpc({ 'knowledge.search': () => ({ hits: [], mode: 'keyword_only', vector_unavailable: null, scan_capped: false }) });
    renderAt(<SearchPreviewTab collection={collection()} />);
    await user.type(screen.getByRole('textbox'), 'xyz');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(await screen.findByText('No matching passages')).toBeInTheDocument();
  });

  it('shows an inline error when the search fails', async () => {
    const user = userEvent.setup();
    mockRpc({});
    renderAt(<SearchPreviewTab collection={collection()} />);
    await user.type(screen.getByRole('textbox'), 'q');
    await user.click(screen.getByRole('button', { name: 'Search' }));
    expect(await screen.findByRole('button', { name: /retry|try again/i })).toBeInTheDocument();
  });

  it('whereText does not repeat a heading the locator already names', () => {
    expect(whereText('「A > B」', 'A > B')).toBe('「A > B」');
    expect(whereText('page 2', 'Leave')).toBe('page 2 · Leave');
    expect(whereText('', 'Leave')).toBe('Leave');
  });

  it('snippet cuts by characters, not UTF-16 units', () => {
    expect(snippet('😀'.repeat(5), 3)).toBe('😀😀😀…');
    expect(snippet('a  b\n c')).toBe('a b c');
  });
});
