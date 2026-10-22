import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import ja from '@/i18n/ja-JP.json';
import type { ExpertDraftPreview, ExpertGenerationJob, ExpertInstallPlacement } from '@/lib/api';
import { aggregateStatus, GenerationProgress } from './GenerationProgress';
import { DraftPreview, warningText } from './DraftPreview';
import { placementProblems } from './InstallResult';

const preview: ExpertDraftPreview = {
  slug: 'shop-team',
  display_name: 'Shop team',
  description: 'Online shop support',
  version: '0.1.0',
  prompts: [],
  channels: ['line'],
  agents: [
    {
      name: 'shop-front',
      role: 'front_desk',
      display_name: 'Front desk',
      reports_to: '',
      soul_excerpt: 'I route requests.',
      skills: [{ name: 'shop-dispatch', description: 'Routes requests to staff' }],
      red_lines: ['decide refunds on behalf of the customer'],
      must_always: ['hand money questions to a human'],
    },
    {
      name: 'shop-returns',
      role: 'worker',
      display_name: 'Returns',
      reports_to: 'shop-front',
      soul_excerpt: 'stand-in',
      placeholder: true,
      skills: [],
      red_lines: [],
    },
  ],
  skills: ['shop-dispatch'],
  wiki_titles: ['SOP', 'FAQ'],
  wiki_pages: [
    { file: 'shop-team/sop.md', title: 'SOP', chars: 900 },
    { file: 'shop-team/faq.md', title: 'FAQ', chars: 900 },
    { file: 'shop-team/glossary.md', title: 'Glossary', chars: 900, web_research: true, content: '# Glossary\nfrom the web' },
  ],
  notes: ['Returns could not be written'],
  install_warnings: [
    { kind: 'wiki', name: 'shop-team/faq.md', reason: 'conflict', detail: '' },
  ],
};

const t = (id: string, values?: Record<string, string | number>) => {
  let s = (en as Record<string, string>)[id] ?? id;
  for (const [k, v] of Object.entries(values ?? {})) s = s.replace(`{${k}}`, String(v));
  return s;
};

function withIntl(node: React.ReactNode) {
  return render(
    <IntlProvider locale="en" messages={en}>
      {node}
    </IntlProvider>,
  );
}

describe('aggregateStatus', () => {
  it('summarises member progress', () => {
    expect(aggregateStatus([])).toBe('pending');
    expect(aggregateStatus(['pending', 'pending'])).toBe('pending');
    expect(aggregateStatus(['done', 'pending'])).toBe('running');
    expect(aggregateStatus(['done', 'failed'])).toBe('done');
    expect(aggregateStatus(['failed', 'failed'])).toBe('failed');
  });
});

describe('GenerationProgress', () => {
  it('lists every stage and each member', () => {
    const job: ExpertGenerationJob = {
      status: 'running',
      kind: 'generate',
      progress: {
        roster: 'done',
        members: [
          { name: 'a', display_name: 'Alice', status: 'done' },
          { name: 'b', display_name: 'Bob', status: 'failed', error: 'x' },
        ],
        wiki: 'running',
        assemble: 'pending',
      },
      started_at: '',
      updated_at: '',
    };
    withIntl(<GenerationProgress job={job} />);
    expect(screen.getByText(en['experts.generate.stage.roster'])).toBeInTheDocument();
    expect(screen.getByText(/\(1\/2\)/)).toBeInTheDocument();
    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.getByText(en['experts.generate.memberFallback'])).toBeInTheDocument();
    expect(screen.getByText(en['experts.generate.stage.wiki'])).toBeInTheDocument();
  });
});

describe('DraftPreview', () => {
  it('shows per-member skills, red lines, pages, notes and install skips', () => {
    withIntl(<DraftPreview preview={preview} />);
    expect(screen.getByText('Routes requests to staff')).toBeInTheDocument();
    expect(screen.getByText('decide refunds on behalf of the customer')).toBeInTheDocument();
    expect(screen.getByText(en['experts.generate.placeholder'])).toBeInTheDocument();
    expect(screen.getByText(en['experts.generate.noSkills'])).toBeInTheDocument();
    expect(screen.getByText('Returns could not be written')).toBeInTheDocument();
    // The skip names the page title, never the internal file path.
    expect(screen.getByText(/already has a page named "FAQ"/)).toBeInTheDocument();
    expect(screen.queryByText(/shop-team\/faq\.md/)).toBeNull();
    // Web-search pages are listed for review with their text.
    expect(screen.getByText(en['experts.generate.webPagesTitle'])).toBeInTheDocument();
    expect(screen.getByText(/from the web/)).toBeInTheDocument();
  });
});

describe('warningText', () => {
  it('falls back to a generic line for unknown warning kinds', () => {
    const has = (id: string) => id in (en as Record<string, string>);
    const known = warningText(
      { kind: 'name', name: 'Front desk', reason: 'conflict', detail: '' },
      preview,
      t,
      has,
    );
    expect(known).toContain('Front desk');
    const unknown = warningText(
      { kind: 'wiki', name: 'shop-team/faq.md', reason: 'mystery' as 'blocked', detail: '' },
      preview,
      t,
      has,
    );
    expect(unknown).toBe(t('experts.generate.warn.generic', { name: 'FAQ' }));
  });
});

describe('placementProblems', () => {
  it('describes everything that was not placed', () => {
    const placement: ExpertInstallPlacement = {
      agents: [
        {
          name: 'shop-front',
          display_name: 'Front desk',
          installed: true,
          skills: [{ name: 'shop-dispatch', installed: false }],
          contract: false,
          renamed: true,
        },
        { name: 'shop-returns', display_name: 'Returns', installed: false, skills: [], contract: null },
      ],
      wiki: [
        { file: 'shop-team/sop.md', status: 'installed' },
        { file: 'shop-team/faq.md', status: 'conflict' },
      ],
      complete: false,
    };
    const lines = placementProblems(placement, preview, t);
    expect(lines).toHaveLength(5);
    expect(lines.join('\n')).toContain('Routes requests to staff');
    expect(lines.join('\n')).toContain('"FAQ"');
    expect(lines.join('\n')).toContain('"Returns" was not installed');
  });
});

describe('expert generation copy', () => {
  it('exists in all three locales', () => {
    const ids = Object.keys(en).filter(
      (k) => k.startsWith('experts.generate.') || k.startsWith('experts.install.result.'),
    );
    for (const catalogue of [zh, ja] as Record<string, string>[]) {
      for (const id of ids) expect(catalogue[id], id).toBeTruthy();
    }
  });
});
