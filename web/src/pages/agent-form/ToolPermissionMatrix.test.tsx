import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import type { BuiltinToolEntry } from '@/lib/api';
import { ToolPermissionMatrix } from './ToolPermissionMatrix';

vi.mock('@/components/shared/ToolCatalogPicker', () => ({ fetchCatalog: vi.fn() }));
import { fetchCatalog } from '@/components/shared/ToolCatalogPicker';

const mcp = (name: string, category: string): BuiltinToolEntry => ({
  name,
  qualified: `mcp__duduclaw__${name}`,
  description: `${name} description`,
  scope: '',
  category,
  kind: 'mcp',
});
const CATALOG = [
  mcp('read_file', 'office'),
  mcp('write_file', 'office'),
  mcp('send_message', 'channel'),
  { name: 'Bash', qualified: 'Bash', description: 'shell', scope: '', category: 'claude', kind: 'claude' },
];

type Props = Partial<React.ComponentProps<typeof ToolPermissionMatrix>>;

function renderMatrix(props: Props = {}) {
  const onApply = vi.fn();
  const utils = render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <ToolPermissionMatrix
        agentName="Quote Bot"
        allowedTools={[]}
        deniedTools={[]}
        approvalRequiredTools={[]}
        isAdmin
        onApply={onApply}
        {...props}
      />
    </IntlProvider>,
  );
  return { onApply, ...utils };
}

// Base UI marks a disabled checkbox with `disabled` and/or `aria-disabled`.
const isDisabled = (el: HTMLElement) =>
  el.hasAttribute('disabled') || el.getAttribute('aria-disabled') === 'true' || el.hasAttribute('data-disabled');

const use = (name: string) => screen.getByRole('checkbox', { name: `${name} can use` });
const approval = (name: string) => screen.getByRole('checkbox', { name: `${name} needs approval` });

beforeEach(() => {
  vi.mocked(fetchCatalog).mockResolvedValue(CATALOG);
});

describe('ToolPermissionMatrix', () => {
  it('shows the catalog by category with N/M can-use counts', async () => {
    renderMatrix({ allowedTools: ['mcp__duduclaw__*'], deniedTools: ['write_file'] });
    await screen.findByText('read_file');
    expect(screen.getByText('1/2 can use')).toBeInTheDocument(); // Documents
    expect(screen.getByText('1/1 can use')).toBeInTheDocument(); // Channel
    expect(use('read_file')).toBeChecked();
    expect(use('write_file')).not.toBeChecked();
    // Native tool is not covered by the MCP wildcard allowlist.
    expect(use('Bash')).not.toBeChecked();
    expect(screen.getByText('read_file description')).toBeInTheDocument();
  });

  it('disables approval while can-use is off, with an explanation', async () => {
    renderMatrix({ deniedTools: ['write_file'] });
    await screen.findByText('write_file');
    expect(isDisabled(approval('write_file'))).toBe(true);
    expect(approval('write_file').closest('span[title]')).toHaveAttribute(
      'title',
      en['agents.perm.matrix.approvalDisabled'],
    );
    expect(isDisabled(approval('read_file'))).toBe(false);
  });

  it('marks a changed row, confirms with counts and a per-tool diff, then applies only the changed lists', async () => {
    const user = userEvent.setup();
    const { onApply } = renderMatrix({ allowedTools: ['mcp__duduclaw__*'], deniedTools: ['read_file'] });
    await screen.findByText('write_file');

    await user.click(use('write_file')); // allowed → not allowed
    await user.click(use('read_file')); // denied → allowed
    await user.click(approval('send_message')); // → needs approval
    expect(screen.getByText('3 unsaved changes')).toBeInTheDocument();
    expect(screen.getAllByText('Changed')).toHaveLength(3);

    await user.click(screen.getByRole('button', { name: 'Save' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Newly allowed 1')).toBeInTheDocument();
    expect(within(dialog).getByText('No longer allowed 1')).toBeInTheDocument();
    expect(within(dialog).getByText('Needs approval 1')).toBeInTheDocument();
    expect(within(dialog).getByText(/Cannot use → Can use$/)).toBeInTheDocument();
    expect(within(dialog).getByText(/Can use → Cannot use$/)).toBeInTheDocument();
    expect(within(dialog).getByText(/Can use → Can use, but approval is needed each time/)).toBeInTheDocument();
    // Nothing is sent before the confirmation.
    expect(onApply).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole('button', { name: 'Confirm and save' }));
    expect(onApply).toHaveBeenCalledTimes(1);
    // allowed_tools (wildcard) is NOT in the payload: it was never touched.
    expect(onApply).toHaveBeenCalledWith({
      denied_tools: ['mcp__duduclaw__write_file'],
      approval_required_tools: ['send_message'],
    });
    expect(screen.queryByText('3 unsaved changes')).not.toBeInTheDocument();
  });

  it('"Back to editing" and "Discard changes" send nothing', async () => {
    const user = userEvent.setup();
    const { onApply } = renderMatrix();
    await screen.findByText('read_file');
    await user.click(use('read_file'));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await user.click(await screen.findByRole('button', { name: 'Back to editing' }));
    await user.click(screen.getByRole('button', { name: 'Discard changes' }));
    expect(screen.queryByText(/unsaved changes/)).not.toBeInTheDocument();
    expect(use('read_file')).toBeChecked();
    expect(onApply).not.toHaveBeenCalled();
  });

  it('group select-all toggles every row of the group', async () => {
    const user = userEvent.setup();
    const { onApply } = renderMatrix();
    await screen.findByText('read_file');
    await user.click(screen.getByRole('checkbox', { name: 'Select all “Documents” as can use' }));
    expect(use('read_file')).not.toBeChecked();
    expect(use('write_file')).not.toBeChecked();
    expect(screen.getByText('0/2 can use')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await user.click(await screen.findByRole('button', { name: 'Confirm and save' }));
    expect(onApply).toHaveBeenCalledWith({
      denied_tools: ['mcp__duduclaw__read_file', 'mcp__duduclaw__write_file'],
    });
  });

  it('search narrows the rows', async () => {
    const user = userEvent.setup();
    renderMatrix();
    await screen.findByText('read_file');
    await user.type(screen.getByRole('textbox', { name: 'Search tools' }), 'send');
    expect(screen.queryByText('read_file')).not.toBeInTheDocument();
    expect(screen.getByText('send_message')).toBeInTheDocument();
  });

  it('locks a tool covered by a wildcard deny (never expands the wildcard)', async () => {
    renderMatrix({ deniedTools: ['mcp__duduclaw__*'] });
    await screen.findByText('read_file');
    expect(isDisabled(use('read_file'))).toBe(true);
    expect(use('read_file').closest('span[title]')).toHaveAttribute('title', en['agents.perm.matrix.allowLocked']);
  });

  it('non-admin: read-only table, lock note, no select-all, no save bar', async () => {
    renderMatrix({ isAdmin: false, deniedTools: ['write_file'], approvalRequiredTools: ['send_message'] });
    await screen.findByText('read_file');
    expect(screen.getByText(/Only an administrator can change tool permissions/)).toBeInTheDocument();
    for (const box of screen.getAllByRole('checkbox')) expect(isDisabled(box)).toBe(true);
    expect(screen.queryByRole('checkbox', { name: /Select all/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument();
    // Current settings are still visible.
    expect(use('write_file')).not.toBeChecked();
    expect(approval('send_message')).toBeChecked();
  });

  it('shows a retryable error when the catalog cannot be read', async () => {
    const user = userEvent.setup();
    vi.mocked(fetchCatalog).mockRejectedValueOnce(new Error('down'));
    renderMatrix();
    expect(await screen.findByText('Can’t load the tool list right now')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByText('read_file')).toBeInTheDocument();
  });
});
