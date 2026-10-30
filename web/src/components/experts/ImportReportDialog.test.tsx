import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import zhTW from '@/i18n/zh-TW.json';
import en from '@/i18n/en.json';
import type { ExpertImportReport, ExpertReimport } from '@/lib/api';
import type { ImportFailure } from './ImportErrorPanel';
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
  it('shows a plan without employee profiles and requires its confirmation digest', () => {
    const plan: ExpertImportReport = {
      ...report,
      details: { profile_install_plan: {
        files: [{ path: 'skills/global/SKILL.md', sha256: 'c'.repeat(64), disposition: 'enabled' }],
      } },
    };
    render(
      <IntlProvider messages={zhTW} locale="zh-TW" defaultLocale="zh-TW">
        <ImportReportDialog open phase="plan" report={plan} busy={false} onConfirm={vi.fn()} onClose={vi.fn()} />
      </IntlProvider>,
    );
    expect(screen.getByRole('heading', { name: '完整安裝計畫' })).toBeInTheDocument();
    expect(screen.getByText(/"skills\/global\/SKILL.md"/)).toHaveTextContent('"disposition": "enabled"');
    expect(screen.getByRole('button', { name: '確認匯入' })).toBeDisabled();
  });

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

  describe('structured failures', () => {
    const empty: ExpertImportReport = {
      dry_run: true,
      meta: {},
      counts: { imported: 0, skipped: 0, conflict: 0, warning: 0, ignored: 0 },
      items: [],
    };

    function renderFailure(
      failure: ImportFailure,
      opts: { report?: ExpertImportReport; onReimport?: (c: ExpertReimport) => void; onConfirm?: () => void } = {},
    ) {
      const onReimport = opts.onReimport ?? vi.fn();
      const onConfirm = opts.onConfirm ?? vi.fn();
      render(
        <IntlProvider messages={zhTW} locale="zh-TW" defaultLocale="zh-TW">
          <ImportReportDialog
            open
            phase="done"
            report={opts.report ?? empty}
            failure={failure}
            busy={false}
            onConfirm={onConfirm}
            onReimport={onReimport}
            onClose={vi.fn()}
          />
        </IntlProvider>,
      );
      return { onReimport, onConfirm };
    }

    it('explains an unrecognised layout with what was found and where a pack sits deeper', () => {
      renderFailure({
        code: 'import.unrecognized',
        params: { entries: ['c/', 'readme.txt'], found: [{ format: 'doubao', path: 'a/b/c/' }], max_unwrap: 2 },
        text: '未識別格式，拒絕安裝',
      });
      expect(screen.getByText('看不出這是哪一種 AI 員工或 AI 團隊的檔案，所以沒有匯入。')).toBeInTheDocument();
      expect(screen.getByText(/最多只會往下找 2 層外層資料夾/)).toBeInTheDocument();
      expect(screen.getByText('在「a/b/c/」找到可以匯入的內容（豆包夥伴安裝包）。')).toBeInTheDocument();
      expect(screen.getByText('readme.txt')).toBeInTheDocument();
      expect(screen.getByText(/錯誤代碼：import.unrecognized/)).toBeInTheDocument();
      // The importer's own wording is not shown for a classified failure.
      expect(screen.queryByText('未識別格式，拒絕安裝')).not.toBeInTheDocument();
    });

    it('offers replace or copy when the upload was imported before', async () => {
      const { onReimport } = renderFailure({
        code: 'import.already_imported',
        params: { slug: 'doubao-a1', name: '報告顧問', agents: ['doubao-a1'], edited: true, edits: [] },
      });
      expect(screen.getByText('這個 AI 團隊之前已經匯入過')).toBeInTheDocument();
      expect(screen.getByText('有 AI 員工在匯入後被修改過，取代前會再請你確認一次。')).toBeInTheDocument();
      await userEvent.click(screen.getByRole('button', { name: '取代之前匯入的版本' }));
      expect(onReimport).toHaveBeenCalledWith({ mode: 'replace', slug: 'doubao-a1' });
      await userEvent.click(screen.getByRole('button', { name: '另存一份副本' }));
      expect(onReimport).toHaveBeenCalledWith({ mode: 'copy' });
    });

    it('lists each employee reason when nothing was imported, and offers a copy on an id clash', async () => {
      const { onReimport } = renderFailure({
        code: 'import.nothing_imported',
        params: { agents: [{ name: 'doubao-a1', code: 'agent.conflict', params: { id: 'doubao-a1' } }] },
      });
      expect(screen.getByText('每位 AI 員工沒有匯入的原因')).toBeInTheDocument();
      expect(screen.getByText(/已有同代號的 AI 員工（doubao-a1），這位沒有匯入/)).toBeInTheDocument();
      expect(screen.queryByText(/上方/)).not.toBeInTheDocument();
      await userEvent.click(screen.getByRole('button', { name: '另存一份副本' }));
      expect(onReimport).toHaveBeenCalledWith({ mode: 'copy' });
    });

    it('shows a timeout and a quota refusal with their numbers', () => {
      renderFailure({ code: 'import.timeout', params: { seconds: 300 } });
      expect(screen.getByText('匯入超過 300 秒還沒完成，已經停止，什麼都沒有改。')).toBeInTheDocument();
    });

    it('renders a quota refusal with current, cap and how many would be added', () => {
      renderFailure({ code: 'import.quota_exceeded', params: { max: 3, current: 2, adding: 2 } });
      expect(screen.getByText(/最多 3 位 AI 員工，已有 2 位，這次要新增 2 位/)).toBeInTheDocument();
    });

    it('asks for a second confirmation when the replace was refused over edits', async () => {
      const { onConfirm } = renderFailure({
        code: 'import.replace_edited',
        params: {
          slug: 'doubao-a1',
          edits: [{ agent: 'doubao-a1', display: '報告顧問', baseline: 'import', changes: [{ path: 'SOUL.md', change: 'modified' }], more: 0 }],
        },
      });
      expect(screen.getByText('人格設定（SOUL.md）')).toBeInTheDocument();
      await userEvent.click(screen.getByRole('button', { name: '我了解，仍要取代' }));
      expect(onConfirm).toHaveBeenCalledWith({ acceptEdits: true });
    });

    it('keeps the raw text folded under details only for unclassified failures', () => {
      renderFailure({ code: 'import.something_new', params: {}, text: 'boom at line 3' });
      expect(screen.getByText('匯入時發生沒有預期到的問題。')).toBeInTheDocument();
      expect(screen.getByText('boom at line 3')).toBeInTheDocument();
      expect(screen.getByText(/錯誤代碼：import.something_new/)).toBeInTheDocument();
    });
  });

  it('a replace plan of edited employees lists the edits before confirming', async () => {
    const plan: ExpertImportReport = {
      ...report,
      details: {
        replace: {
          slug: 'doubao-a1',
          name: '報告顧問',
          agents: ['doubao-a1'],
          edited: true,
          edits: [{ agent: 'doubao-a1', display: '報告顧問', baseline: 'import', changes: [{ path: 'notes.md', change: 'added' }], more: 2 }],
        },
      },
    };
    const onConfirm = vi.fn();
    render(
      <IntlProvider messages={zhTW} locale="zh-TW" defaultLocale="zh-TW">
        <ImportReportDialog
          open
          phase="plan"
          report={plan}
          reimport={{ mode: 'replace', slug: 'doubao-a1' }}
          busy={false}
          onConfirm={onConfirm}
          onClose={vi.fn()}
        />
      </IntlProvider>,
    );
    expect(screen.getByText(/這次會取代之前匯入的「報告顧問」（1 位 AI 員工）/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: '確認取代' }));
    expect(onConfirm).not.toHaveBeenCalled();
    expect(screen.getByText('這些修改會被換掉')).toBeInTheDocument();
    expect(screen.getByText('notes.md')).toBeInTheDocument();
    expect(screen.getByText('還有 2 個檔案')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: '我了解，仍要取代' }));
    expect(onConfirm).toHaveBeenCalledWith({ acceptEdits: true });
  });
});
