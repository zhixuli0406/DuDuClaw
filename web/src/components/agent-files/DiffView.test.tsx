import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import en from '@/i18n/en.json';
import { renderWithProviders } from '@/test/render';
import { DiffView, diffRows } from './DiffView';

const unchanged = Array.from({ length: 20 }, (_, i) => `第 ${i + 1} 行：保持不變`);

describe('<DiffView>', () => {
  it('shows added and removed CJK lines as text with markers', () => {
    renderWithProviders(
      <DiffView
        hunks={[
          { op: 'equal', lines: ['# 人格設定'] },
          { op: 'remove', lines: ['請保持禮貌。'] },
          { op: 'add', lines: ['請保持親切，用繁體中文回答。<b>不是標籤</b>'] },
        ]}
      />,
    );
    const list = screen.getByTestId('agent-file-diff');
    expect(list.querySelector('[data-diff="remove"]')).toHaveTextContent('請保持禮貌。');
    const added = list.querySelector('[data-diff="add"]');
    expect(added).toHaveTextContent('請保持親切，用繁體中文回答。<b>不是標籤</b>');
    expect(added?.querySelector('b')).toBeNull();
    expect(screen.getByText(`${en['agentFiles.approval.addedLine']}:`, { exact: false })).toBeInTheDocument();
  });

  it('collapses a long unchanged run and expands it on click', async () => {
    renderWithProviders(
      <DiffView hunks={[{ op: 'add', lines: ['新的一行'] }, { op: 'equal', lines: unchanged }, { op: 'remove', lines: ['舊的一行'] }]} />,
    );
    const collapsed = screen.getByRole('button', { name: '16 unchanged lines' });
    expect(screen.queryByText('第 10 行：保持不變')).not.toBeInTheDocument();
    expect(screen.getByText('第 1 行：保持不變')).toBeInTheDocument();
    await userEvent.click(collapsed);
    expect(screen.getByText('第 10 行：保持不變')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /unchanged lines/ })).not.toBeInTheDocument();
  });

  it('says so when nothing differs', () => {
    renderWithProviders(<DiffView hunks={[]} />);
    expect(screen.getByText(en['agentFiles.diff.noChanges'])).toBeInTheDocument();
  });

  it('keeps the flat row shape for the approval card', () => {
    const rows = diffRows([{ op: 'equal', lines: unchanged }]);
    expect(rows.filter((r) => r.kind === 'collapsed')).toEqual([{ kind: 'collapsed', count: 20 }]);
  });
});
