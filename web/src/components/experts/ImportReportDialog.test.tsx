import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import en from '@/i18n/en.json';
import type { ExpertImportReport } from '@/lib/api';
import { ImportReportDialog } from './ImportReportDialog';

const report: ExpertImportReport = {
  dry_run: true,
  meta: { format: 'doubao', slug: 'doubao-a1', disabled_skills: '1' },
  counts: { imported: 3, skipped: 0, conflict: 0, warning: 1, ignored: 1 },
  items: [
    { status: 'imported', kind: 'format', name: '豆包夥伴安裝包', detail: '' },
    { status: 'imported', kind: 'agent', name: '報告顧問（doubao-a1）', detail: '', code: 'agent.created', params: { display: '報告顧問', id: 'doubao-a1' } },
    { status: 'imported', kind: 'instructions', name: 'agents/a1/persona/AGENTS.md → 工作規則（INSTRUCTIONS.md，AI 員工只能讀）', detail: '' },
    {
      status: 'warning',
      kind: 'skill-disabled',
      name: '飛書畫板大師（beautiful-feishu-whiteboard）',
      detail: '先停用，待管理者核准：內含可執行腳本（1 個）',
      code: 'skill.quarantined',
      params: { skill: '飛書畫板大師', scripts: 1, needs: ['feishu'] },
    },
    { status: 'ignored', kind: 'archive', name: 'README.md', detail: '安裝包說明文件，只保存不匯入' },
  ],
};

function renderDialog(phase: 'plan' | 'done', onConfirm = vi.fn(), error?: string, messages: Record<string, string> = zhTW, locale = 'zh-TW') {
  render(
    <IntlProvider messages={messages} locale={locale} defaultLocale="zh-TW">
      <ImportReportDialog open phase={phase} report={report} error={error} busy={false} onConfirm={onConfirm} onClose={vi.fn()} />
    </IntlProvider>,
  );
  return onConfirm;
}

describe('ImportReportDialog', () => {
  it('groups the plan in plain language and confirms on request', async () => {
    const onConfirm = renderDialog('plan');
    expect(screen.getByText('匯入前確認')).toBeInTheDocument();
    expect(screen.getByText(/偵測到「豆包夥伴安裝包」/)).toBeInTheDocument();
    expect(screen.getByText('1 個技能會先停用，等管理者核准後才會加到 AI 員工身上。')).toBeInTheDocument();
    // Section headings and translated kind / status codes — never raw codes.
    expect(screen.getByText('檔案去處')).toBeInTheDocument();
    expect(screen.getByText('工作規則')).toBeInTheDocument();
    expect(screen.getByText('停用的技能')).toBeInTheDocument();
    expect(screen.queryByText('skill-disabled')).not.toBeInTheDocument();
    // Coded lines render from the code; the importer's wording is under 詳細.
    expect(screen.getByText(/技能「飛書畫板大師」先停用，等管理者核准（腳本 1 個；需要的外部能力：飛書（Lark）文件與 API）/)).toBeInTheDocument();
    expect(screen.getByText('建立 AI 員工「報告顧問」')).toBeInTheDocument();
    expect(screen.getAllByText('詳細').length).toBeGreaterThan(0);
    await userEvent.click(screen.getByRole('button', { name: '確認匯入' }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it('shows the result with a single close action', () => {
    renderDialog('done');
    expect(screen.getByText('匯入完成')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '確認匯入' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: '完成' })).toBeInTheDocument();
  });

  it('renders coded lines in the viewer language', () => {
    renderDialog('plan', vi.fn(), undefined, en, 'en');
    expect(screen.getByText('Create AI staff member “報告顧問”')).toBeInTheDocument();
    expect(screen.getByText(/stays off until an administrator approves it \(1 scripts; outside services needed: Feishu/)).toBeInTheDocument();
  });

  it('shows a failure with its reason and only a close action', () => {
    renderDialog('done', vi.fn(), '安裝失敗：未識別格式');
    expect(screen.getByText('匯入沒有完成')).toBeInTheDocument();
    expect(screen.getByText('安裝失敗：未識別格式')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '確認匯入' })).not.toBeInTheDocument();
  });
});
