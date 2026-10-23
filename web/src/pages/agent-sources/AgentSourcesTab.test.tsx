import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { callsOf, collection, mockRpc, renderAt, source } from '@/pages/knowledge/test-fixtures';
import type { KbAgentDataSources, KbAgentTabCollection } from '@/lib/knowledge-api';
import { AgentSourcesTab } from './AgentSourcesTab';

beforeEach(() => vi.clearAllMocks());

function tabCollection(over: Partial<KbAgentTabCollection> = {}): KbAgentTabCollection {
  return {
    id: 'col_1',
    slug: 'hr',
    name: 'HR handbook',
    sensitivity: 'internal',
    source_count: 2,
    ready_count: 2,
    chunk_count: 9,
    in_use: true,
    is_own: false,
    my_role: null,
    readers: null,
    reader_counts: { users: 3, departments: 1, agents: 0, channels: 0 },
    ...over,
  };
}

const OWN = tabCollection({
  id: 'col_own',
  slug: 'agent-a1',
  name: 'Sales helper 的資料',
  is_own: true,
  my_role: 'owner',
  readers: [
    { kind: 'user', id: 'u1', label: 'Mei', role: 'owner', you: true },
    { kind: 'user', id: 'u2', label: 'Ken', role: 'viewer', you: false },
  ],
  reader_counts: { users: 2, departments: 0, agents: 0, channels: 0 },
});

function data(over: Partial<KbAgentDataSources> = {}): KbAgentDataSources {
  return {
    agent_id: 'a1',
    binding: 'all',
    collections: [tabCollection()],
    own: null,
    can_manage: false,
    manage_blocked: 'not_owner',
    default_readers: [{ id: 'u2', label: 'Ken', role: 'viewer' }],
    ...over,
  };
}

const render = () => renderAt(<AgentSourcesTab agentId="a1" agentName="Sales helper" />);

describe('AgentSourcesTab', () => {
  it('viewers see what the employee uses, the privacy rule, and no add button', async () => {
    mockRpc({ 'knowledge.agent.data_sources': () => data() });
    render();
    expect(await screen.findByText('No files of its own yet')).toBeInTheDocument();
    expect(screen.getByText(/only get passages they may read themselves/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Add files' })).toBeNull();
    const shared = screen.getByTestId('agent-shared-collections');
    expect(within(shared).getByText('HR handbook')).toBeInTheDocument();
    expect(within(shared).getByText('Who can read: 3 colleagues and 1 department, and admins.')).toBeInTheDocument();
    // Not a reader: no link into the library.
    expect(within(shared).queryByRole('button', { name: /Open collection/ })).toBeNull();
  });

  it('the first upload creates the own collection with the chosen sharing, then opens the upload dialog', async () => {
    const user = userEvent.setup();
    mockRpc({
      'knowledge.agent.data_sources': () => data({ can_manage: true, manage_blocked: null }),
      'knowledge.agent.own.ensure': () => ({
        collection: collection({ id: 'col_own', name: 'Sales helper 的資料' }),
        created: true,
        binding_changed: false,
      }),
      'knowledge.limits': () => ({ max_source_bytes: 1048576, max_files_per_upload: 20, extensions: ['pdf'] }),
    });
    render();
    await screen.findByText('No files of its own yet');
    await user.click(screen.getByRole('button', { name: 'Add files' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Ken')).toBeInTheDocument();
    await user.selectOptions(within(dialog).getByLabelText('Sensitivity'), 'personal');
    await user.click(within(dialog).getByRole('checkbox', { name: /Also let the colleagues/ }));
    await user.click(within(dialog).getByRole('button', { name: 'Create and choose files' }));
    await waitFor(() =>
      expect(callsOf('knowledge.agent.own.ensure')).toEqual([
        { agent_id: 'a1', sensitivity: 'personal', share_chat_users: false },
      ]),
    );
    expect(await screen.findByText('Add to “Sales helper 的資料”. The same file is never added twice.')).toBeInTheDocument();
  });

  it('lists its own files with reading state and removes one with the forget option', async () => {
    const user = userEvent.setup();
    let sources = [
      source({ id: 's1', collection_id: 'col_own', title: 'prices.xlsx', status: 'ready', status_detail: 'hybrid' }),
      source({
        id: 's2',
        collection_id: 'col_own',
        title: 'scan.pdf',
        status: 'failed',
        error_message: '這份 PDF 沒有文字層，請先轉成文字再上傳。',
      }),
    ];
    mockRpc({
      'knowledge.agent.data_sources': () =>
        data({
          collections: [OWN, tabCollection()],
          own: { collection: OWN, archived: false, share_chat_users: true, sources },
          can_manage: true,
          manage_blocked: null,
        }),
      'knowledge.sources.delete': () => {
        sources = sources.filter((s) => s.id !== 's1');
        return { deleted: true, forget: { conversations: 1, filed: [{}], nothing_to_forget: 0, failed: [], skipped: 0 } };
      },
    });
    render();
    expect(await screen.findByText('prices.xlsx')).toBeInTheDocument();
    expect(screen.getByText('這份 PDF 沒有文字層，請先轉成文字再上傳。')).toBeInTheDocument();
    expect(screen.getByText('Who can read: you and Ken, and admins.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Manage in the document library/ })).toBeInTheDocument();
    // The own collection is not repeated under "shared by others".
    expect(within(screen.getByTestId('agent-shared-collections')).queryByText('Sales helper 的資料')).toBeNull();

    await user.click(screen.getByRole('button', { name: 'Actions for “prices.xlsx”' }));
    await user.click(await screen.findByRole('menuitem', { name: 'Remove' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('checkbox', { name: /forget what it learned/ }));
    await user.click(within(dialog).getByRole('button', { name: 'Remove' }));
    await waitFor(() =>
      expect(callsOf('knowledge.sources.delete')).toEqual([{ source_id: 's1', also_forget_memories: true }]),
    );
    await waitFor(() => expect(screen.queryByText('prices.xlsx')).toBeNull());
  });

  it('shows embedding progress while a file is still being read', async () => {
    const busy = source({
      id: 's3',
      title: 'spec.docx',
      status: 'embedding',
      job: { kind: 'embed', state: 'running', attempts: 1, progress_done: 2, progress_total: 5 },
    });
    mockRpc({
      'knowledge.agent.data_sources': () =>
        data({ collections: [OWN], own: { collection: OWN, archived: false, share_chat_users: true, sources: [busy] } }),
    });
    render();
    expect(await screen.findByText('Analysing meaning 40%')).toBeInTheDocument();
  });

  it('explains why a later owner, a non-reader or an archived employee cannot change files', async () => {
    const notReader = { ...OWN, my_role: null, readers: null };
    mockRpc({
      'knowledge.agent.data_sources': () =>
        data({
          collections: [notReader],
          own: { collection: notReader, archived: false, share_chat_users: true, sources: null },
          manage_blocked: 'not_collection_editor',
        }),
    });
    const view = render();
    expect(await screen.findByText(/Another owner manages these files/)).toBeInTheDocument();
    expect(screen.getByText('You may not read these files, so their names are not shown.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Add files' })).toBeNull();
    view.unmount();

    mockRpc({
      'knowledge.agent.data_sources': () =>
        data({
          collections: [],
          own: { collection: OWN, archived: true, share_chat_users: true, sources: [] },
          manage_blocked: 'archived',
          binding: 'off',
        }),
    });
    render();
    expect(await screen.findByText(/This employee is archived/)).toBeInTheDocument();
    expect(screen.getByText('Archived with the employee')).toBeInTheDocument();
    expect(screen.getByText(/switch the document library off/)).toBeInTheDocument();
  });

  it('a load failure offers a retry', async () => {
    mockRpc({});
    render();
    expect(await screen.findByRole('button', { name: /retry|try again/i })).toBeInTheDocument();
  });
});
