import { describe, it, expect, vi, afterEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import en from '@/i18n/en.json';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import type { AgentFileListEntry, AgentFileSaveResponse } from '@/lib/agent-files-types';

vi.mock('@/lib/ws-client', () => ({
  client: { call: vi.fn().mockResolvedValue({}), subscribe: vi.fn() },
}));

import { client } from '@/lib/ws-client';
import { AgentFilesTab } from './AgentFilesTab';

type Role = 'admin' | 'owner' | 'operator' | 'viewer';

function loginAs(role: Role) {
  useAuthStore.setState({
    user: { id: 'u1', email: 'u@example.invalid', display_name: 'U', role: role === 'admin' ? 'admin' : 'employee', status: 'active' },
    bindings: role === 'admin' ? [] : [{ user_id: 'u1', agent_name: 'a1', access_level: role, bound_at: '' }],
  } as never);
}

function entry(file: string, over: Partial<AgentFileListEntry> = {}): AgentFileListEntry {
  return {
    file: file as AgentFileListEntry['file'],
    label_key: file.startsWith('skill:') ? 'agentFiles.file.skill' : `agentFiles.file.${file}`,
    exists: true,
    size: 10,
    hash: `h-${file}`,
    editable_by_me: true,
    last_change: null,
    drift: file === 'soul' || file === 'instructions' ? 'intact' : 'none',
    pending_approval_id: null,
    ...over,
  };
}

interface Setup {
  editable?: boolean;
  soul?: Partial<AgentFileListEntry>;
  /** INSTRUCTIONS.md is absent on a fresh employee. */
  noInstructions?: boolean;
  runtimeFiles?: Array<{ name: string; content: string; size: number; truncated: boolean }> | 'fail';
  drift?: { state: 'intact' | 'drifted' | 'none'; accepted_version_id: string | null };
  save?: (params: Record<string, unknown>) => AgentFileSaveResponse | Promise<AgentFileSaveResponse>;
}

const SOUL = '我是 Alice，親切的客服。';

function mockGateway({ editable = true, soul = {}, noInstructions = false, runtimeFiles = [], drift = { state: 'intact', accepted_version_id: null }, save }: Setup = {}) {
  let current = SOUL;
  const calls: Array<{ method: string; params: Record<string, unknown> }> = [];
  vi.mocked(client.call).mockImplementation(async (method: string, params?: Record<string, unknown>) => {
    const p = params ?? {};
    calls.push({ method, params: p });
    switch (method) {
      case 'agent_files.list':
        return {
          agent_id: 'a1',
          files: [
            entry('soul', { editable_by_me: editable, ...soul }),
            entry('identity', { editable_by_me: editable }),
            entry('instructions', { editable_by_me: editable, exists: !noInstructions, size: noInstructions ? 0 : 10 }),
            entry('contract', { editable_by_me: editable }),
            entry('agent_toml', { editable_by_me: false }),
            entry('skill:quote/SKILL.md', { editable_by_me: editable, skill_name: 'quote', path: 'SKILLS/quote/SKILL.md' }),
          ],
          limits: { soul: 65536, identity: 65536, instructions: 65536, contract: 32768, agent_toml: 262144, skill: 65536 },
        };
      case 'agent_files.get':
        return {
          agent_id: 'a1',
          file: p.file,
          content: p.file === 'soul' ? current : p.file === 'instructions' && noInstructions ? '' : `內容 ${String(p.file)}`,
          hash: p.file === 'soul' ? `h:${current}` : `h-${String(p.file)}`,
          exists: !(p.file === 'instructions' && noInstructions),
          masked: [],
          drift: p.file === 'soul' ? drift : { state: 'none', accepted_version_id: null },
        };
      case 'agent_files.runtime_files':
        if (runtimeFiles === 'fail') throw new Error('boom');
        return { agent_id: 'a1', files: runtimeFiles };
      case 'agent_files.diff':
        return { added: 1, removed: 1, hunks: [{ op: 'remove', lines: [SOUL] }, { op: 'add', lines: ['新的人格'] }] };
      case 'agent_files.save':
      case 'agent_files.rollback': {
        const res = save ? await save(p) : ({ status: 'applied', file: p.file, agent_id: 'a1', hash: 'new' } as AgentFileSaveResponse);
        if (res.status === 'applied' && typeof p.content === 'string') current = p.content;
        return res;
      }
      default:
        return {};
    }
  });
  return {
    calls,
    setCurrent: (s: string) => {
      current = s;
    },
  };
}

async function openTab() {
  renderWithProviders(<AgentFilesTab agentId="a1" />);
  await screen.findByTestId('agent-files-tab');
  await screen.findByTestId('agent-file-content');
}

async function editSoul(text: string) {
  await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.edit'] }));
  const box = screen.getByRole('textbox', { name: 'Edit Personality' });
  await userEvent.clear(box);
  await userEvent.type(box, text);
}

describe('<AgentFilesTab>', () => {
  afterEach(() => {
    vi.mocked(client.call).mockReset();
    useAuthStore.setState({ user: null, bindings: [] } as never);
  });

  it('lists the files in plain language with small file names and skills', async () => {
    loginAs('owner');
    mockGateway();
    await openTab();
    const nav = screen.getByRole('navigation', { name: en['agentFiles.list.label'] });
    for (const id of ['soul', 'identity', 'contract', 'agent_toml'] as const) {
      expect(within(nav).getByText(en[`agentFiles.file.${id}`])).toBeInTheDocument();
    }
    // Work rules carry the industry names people search for.
    expect(within(nav).getByText(en['agentFiles.file.instructionsFull'])).toBeInTheDocument();
    expect(within(nav).getByText('INSTRUCTIONS.md')).toBeInTheDocument();
    expect(within(nav).getByText('SOUL.md')).toBeInTheDocument();
    expect(within(nav).getByText('Skill: quote')).toBeInTheDocument();
    expect(within(nav).getByText('SKILLS/quote/SKILL.md')).toBeInTheDocument();
    expect(within(nav).getByText(en['agentFiles.badge.readOnly'])).toBeInTheDocument();
    expect(screen.getByTestId('agent-file-content')).toHaveTextContent(SOUL);
  });

  it('a viewer sees the text, the hint and no edit or history buttons', async () => {
    loginAs('viewer');
    mockGateway({ editable: false });
    await openTab();
    expect(screen.getByTestId('view-only-hint')).toHaveTextContent(en['agentFiles.editor.viewOnly']);
    expect(screen.queryByRole('button', { name: en['agentFiles.editor.edit'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: en['agentFiles.editor.history'] })).not.toBeInTheDocument();
    expect(screen.getAllByText(en['agentFiles.badge.viewOnly']).length).toBeGreaterThan(0);
  });

  it('an operator may open the history but not edit', async () => {
    loginAs('operator');
    mockGateway({ editable: false });
    await openTab();
    expect(screen.getByRole('button', { name: en['agentFiles.editor.history'] })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: en['agentFiles.editor.edit'] })).not.toBeInTheDocument();
  });

  it('the owner edits, previews the change and saves through the check', async () => {
    loginAs('owner');
    const gw = mockGateway();
    await openTab();
    await editSoul('新的人格');
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.showDiff'] }));
    expect(await screen.findByTestId('agent-file-diff')).toHaveTextContent('新的人格');
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.saveAndCheck'] }));
    await waitFor(() => expect(screen.getByTestId('agent-file-content')).toHaveTextContent('新的人格'));
    const save = gw.calls.find((c) => c.method === 'agent_files.save');
    expect(save?.params).toMatchObject({ agent_id: 'a1', file: 'soul', content: '新的人格', base_hash: `h:${SOUL}` });
  });

  it('asks before an unsaved edit is lost', async () => {
    loginAs('owner');
    mockGateway();
    await openTab();
    await editSoul('改到一半');
    await userEvent.click(screen.getByTestId('agent-file-identity'));
    const dialog = await screen.findByTestId('unsaved-changes-dialog');
    await userEvent.click(within(dialog).getByRole('button', { name: en['agentFiles.unsaved.stay'] }));
    expect(screen.getByRole('textbox', { name: 'Edit Personality' })).toHaveValue('改到一半');
    await userEvent.click(screen.getByTestId('agent-file-identity'));
    await userEvent.click(within(await screen.findByTestId('unsaved-changes-dialog')).getByRole('button', { name: en['agentFiles.unsaved.discard'] }));
    await waitFor(() => expect(screen.getByTestId('agent-file-content')).toHaveTextContent('內容 identity'));
  });

  it('a warning opens the review dialog and confirm resends the same change', async () => {
    loginAs('owner');
    const gw = mockGateway({
      save: (p) =>
        p.action === 'confirm'
          ? ({ status: 'applied', file: 'soul', agent_id: 'a1', hash: 'n' } as AgentFileSaveResponse)
          : ({
              status: 'needs_confirmation',
              file: 'soul',
              agent_id: 'a1',
              review: {
                analysis_id: 'an-1',
                verdict: 'warn',
                basis: 'rules+ai',
                summary: '',
                reasons: [{ category: 'role_drift', severity: 'medium', message: 'x', evidence: '' }],
                can_confirm: true,
                can_request_approval: true,
              },
            } as AgentFileSaveResponse),
    });
    await openTab();
    await editSoul('完全不同的角色');
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.saveAndCheck'] }));
    const dialog = await screen.findByTestId('change-review-dialog');
    expect(within(dialog).getByText(en['agentFiles.category.role_drift'])).toBeInTheDocument();
    await userEvent.type(within(dialog).getByRole('textbox'), '新的服務方向');
    await userEvent.click(within(dialog).getByRole('button', { name: en['agentFiles.review.confirm'] }));
    await waitFor(() => expect(gw.calls.some((c) => c.method === 'agent_files.save' && c.params.action === 'confirm')).toBe(true));
    const confirm = gw.calls.find((c) => c.params.action === 'confirm');
    expect(confirm?.params).toMatchObject({ content: '完全不同的角色', analysis_id: 'an-1', reason: '新的服務方向', base_hash: `h:${SOUL}` });
  });

  it('a conflict shows both texts side by side and reload brings in the latest', async () => {
    loginAs('owner');
    const gw = mockGateway({
      save: () => {
        gw.setCurrent('別人剛存的版本');
        return { status: 'conflict', file: 'soul', agent_id: 'a1', current_hash: 'other' } as AgentFileSaveResponse;
      },
    });
    await openTab();
    await editSoul('我的版本');
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.saveAndCheck'] }));
    expect(await screen.findByTestId('conflict-mine')).toHaveTextContent('我的版本');
    expect(screen.getByTestId('conflict-current')).toHaveTextContent('別人剛存的版本');
    expect(screen.getByRole('button', { name: en['agentFiles.conflict.copyMine'] })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.review.reload'] }));
    expect(await screen.findByTestId('agent-file-content')).toHaveTextContent('別人剛存的版本');
  });

  it('a drifted file offers restore and check-and-accept', async () => {
    loginAs('owner');
    const gw = mockGateway({ soul: { drift: 'drifted' }, drift: { state: 'drifted', accepted_version_id: '1730000000001-aaaaaaaaaaaa' } });
    await openTab();
    expect(screen.getByText(en['agentFiles.badge.drifted'])).toBeInTheDocument();
    const banner = screen.getByTestId('drift-banner');
    await userEvent.click(within(banner).getByRole('button', { name: en['agentFiles.drift.accept'] }));
    await waitFor(() => expect(gw.calls.some((c) => c.method === 'agent_files.save')).toBe(true));
    expect(gw.calls.find((c) => c.method === 'agent_files.save')?.params).toMatchObject({ content: SOUL, base_hash: `h:${SOUL}` });
    await waitFor(() => expect(screen.getByTestId('drift-banner')).toBeInTheDocument());
    await userEvent.click(within(screen.getByTestId('drift-banner')).getByRole('button', { name: en['agentFiles.drift.restore'] }));
    await waitFor(() => expect(gw.calls.some((c) => c.method === 'agent_files.rollback')).toBe(true));
    expect(gw.calls.find((c) => c.method === 'agent_files.rollback')?.params).toMatchObject({ version_id: '1730000000001-aaaaaaaaaaaa' });
  });

  it('a waiting request shows its badge and notice', async () => {
    loginAs('owner');
    mockGateway({ soul: { pending_approval_id: 'apr-1' } });
    await openTab();
    expect(screen.getByText(en['agentFiles.badge.pending'])).toBeInTheDocument();
    expect(screen.getByText(en['agentFiles.pending.notice'])).toBeInTheDocument();
    expect(screen.getByRole('button', { name: en['agentFiles.pending.withdraw'] })).toBeInTheDocument();
  });

  it('basic settings are read-only with a way to the edit page', async () => {
    loginAs('admin');
    mockGateway();
    await openTab();
    await userEvent.click(screen.getByTestId('agent-file-agent_toml'));
    expect(await screen.findByText(en['agentFiles.editor.basicReadOnly'])).toBeInTheDocument();
    expect(screen.getByRole('button', { name: en['agentFiles.editor.openEditPage'] })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: en['agentFiles.editor.edit'] })).not.toBeInTheDocument();
  });

  it('a skill opens and saves like the other files', async () => {
    loginAs('owner');
    const gw = mockGateway();
    await openTab();
    await userEvent.click(screen.getByTestId('agent-file-skill:quote/SKILL.md'));
    await waitFor(() => expect(screen.getByTestId('agent-file-content')).toHaveTextContent('內容 skill:quote/SKILL.md'));
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.edit'] }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Edit Skill: quote' }), '，再列出方案');
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.saveAndCheck'] }));
    await waitFor(() => expect(gw.calls.some((c) => c.method === 'agent_files.save' && c.params.file === 'skill:quote/SKILL.md')).toBe(true));
  });

  it('work rules explain where imported AGENTS.md / CLAUDE.md content lives', async () => {
    loginAs('owner');
    mockGateway();
    await openTab();
    await userEvent.click(screen.getByTestId('agent-file-instructions'));
    expect(await screen.findByTestId('instructions-help')).toHaveTextContent(en['agentFiles.help.instructions']);
    expect(screen.getByRole('heading', { name: en['agentFiles.file.instructionsFull'] })).toBeInTheDocument();
  });

  it('offers to create missing work rules and saves through the ordinary save path', async () => {
    loginAs('owner');
    const gw = mockGateway({ noInstructions: true });
    renderWithProviders(<AgentFilesTab agentId="a1" />);
    await screen.findByTestId('agent-files-tab');
    await userEvent.click(screen.getByTestId('agent-file-instructions'));
    await screen.findByTestId('instructions-missing');
    // Only one way to start: the header edit button is not shown.
    expect(screen.queryByRole('button', { name: en['agentFiles.editor.edit'] })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.instructions.create'] }));
    const box = screen.getByRole('textbox', { name: 'Edit Work rules' });
    expect(box).toHaveValue(en['agentFiles.instructions.template']);
    await userEvent.click(screen.getByRole('button', { name: en['agentFiles.editor.saveAndCheck'] }));
    await waitFor(() => expect(gw.calls.some((c) => c.method === 'agent_files.save')).toBe(true));
    expect(gw.calls.find((c) => c.method === 'agent_files.save')?.params).toMatchObject({
      agent_id: 'a1',
      file: 'instructions',
      content: en['agentFiles.instructions.template'],
      base_hash: 'h-instructions',
    });
  });

  it('a viewer sees that work rules are missing but cannot create them', async () => {
    loginAs('viewer');
    mockGateway({ editable: false, noInstructions: true });
    renderWithProviders(<AgentFilesTab agentId="a1" />);
    await screen.findByTestId('agent-files-tab');
    await userEvent.click(screen.getByTestId('agent-file-instructions'));
    await screen.findByTestId('instructions-missing');
    expect(screen.getByText(en['agentFiles.instructions.createViewOnly'])).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: en['agentFiles.instructions.create'] })).not.toBeInTheDocument();
  });

  it('shows the generated runtime files read-only, loaded only when opened', async () => {
    loginAs('owner');
    const gw = mockGateway({ runtimeFiles: [{ name: 'AGENTS.md', content: '# Alice 的執行環境檔', size: 20, truncated: false }] });
    await openTab();
    expect(gw.calls.some((c) => c.method === 'agent_files.runtime_files')).toBe(false);
    const section = screen.getByTestId('runtime-files-section');
    expect(within(section).getByText(en['agentFiles.runtime.title'])).toBeInTheDocument();
    await userEvent.click(within(section).getByText(en['agentFiles.runtime.title']));
    const file = await screen.findByTestId('runtime-file-AGENTS.md');
    expect(file).toHaveTextContent('# Alice 的執行環境檔');
    expect(within(section).getByText(en['agentFiles.runtime.note'])).toBeInTheDocument();
    expect(within(section).queryByRole('textbox')).not.toBeInTheDocument();
    expect(gw.calls.filter((c) => c.method === 'agent_files.runtime_files')).toHaveLength(1);
  });

  it('says so when the generated files cannot be read', async () => {
    loginAs('owner');
    mockGateway({ runtimeFiles: 'fail' });
    await openTab();
    await userEvent.click(screen.getByText(en['agentFiles.runtime.title']));
    expect(await screen.findByText(en['agentFiles.runtime.failed'])).toBeInTheDocument();
  });
});
