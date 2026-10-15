import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { SubscriptionRiskDisclosure, SubscriptionRiskNote } from './SubscriptionRiskDisclosure';
import zhTW from '@/i18n/zh-TW.json';
import en from '@/i18n/en.json';
import ja from '@/i18n/ja-JP.json';

const LEGAL = 'https://code.claude.com/docs/en/legal-and-compliance';

describe('SubscriptionRiskDisclosure', () => {
  it('states the policy, the conflicting uses and links the primary source', () => {
    renderWithProviders(<SubscriptionRiskDisclosure checked={false} onCheckedChange={vi.fn()} />);
    expect(screen.getByText(/route requests through Free, Pro, or Max plan credentials/i)).toBeInTheDocument();
    expect(screen.getByText(/rotating several subscription accounts/i)).toBeInTheDocument();
    expect(screen.getByText(/having DuDuClaw store your token/i)).toBeInTheDocument();
    expect(screen.getByText(/may not make your account available to anyone else/i)).toBeInTheDocument();
    expect(screen.getByText(/without prior notice/i)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /legal and compliance/i })).toHaveAttribute('href', LEGAL);
  });

  it('reports the checkbox state to the parent', async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderWithProviders(<SubscriptionRiskDisclosure checked={false} onCheckedChange={onChange} />);
    await user.click(screen.getByRole('checkbox', { name: /read the policy and risks/i }));
    expect(onChange).toHaveBeenCalledWith(true, expect.anything());
  });

  it('renders the one-line note with the same link', () => {
    renderWithProviders(<SubscriptionRiskNote />);
    expect(screen.getByRole('link', { name: /read the policy/i })).toHaveAttribute('href', LEGAL);
  });
});

/** Copy guard: the Anthropic sentences state policy, never a technical block
 *  (owner decision 2026-10-16). Only the kept Google sentence (`.others`)
 *  may say "blocked". zh-TW also follows the house writing rules. */
describe('subscription risk copy', () => {
  const anthropicKeys = [
    'subscriptionRisk.policy',
    'subscriptionRisk.conflicts',
    'subscriptionRisk.enforcement',
    'subscriptionRisk.recommendation',
    'subscriptionRisk.short',
    'welcome.backend.claudeSub.risk',
  ];
  const catalogs: Record<string, Record<string, string>> = {
    'zh-TW': zhTW as Record<string, string>,
    en: en as Record<string, string>,
    'ja-JP': ja as Record<string, string>,
  };

  it.each(Object.keys(catalogs))('%s never claims Anthropic blocks it server-side', (loc) => {
    for (const key of anthropicKeys) {
      const text = catalogs[loc][key];
      expect(text, `${loc} ${key}`).toBeTruthy();
      expect(text).not.toMatch(/封鎖|伺服器端|blocked|server side|server-side|ブロック|サーバー側/i);
    }
  });

  it('drops the old notice key everywhere', () => {
    for (const cat of Object.values(catalogs)) {
      expect(cat['subscriptionRisk.notice']).toBeUndefined();
    }
  });

  it('zh-TW risk copy avoids the 不是…而是 pattern and em dashes', () => {
    const zh = catalogs['zh-TW'];
    for (const [key, text] of Object.entries(zh)) {
      if (!key.startsWith('subscriptionRisk.') && key !== 'welcome.backend.claudeSub.risk') continue;
      expect(text, key).not.toMatch(/不是.*而是|——/);
    }
  });
});
