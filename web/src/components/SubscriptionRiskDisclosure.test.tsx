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

  it.each([
    ['openai', /Sign in with ChatGPT terms/, 'https://openai.com/policies/sign-in-with-chatgpt-terms/', /official Sign in with ChatGPT program/],
    ['gemini', /Gemini CLI terms of service/, 'https://geminicli.com/docs/resources/tos-privacy/', /no longer accepts Login with Google/],
    ['xai', /xAI Terms of Service/, 'https://x.ai/legal/terms-of-service', /SuperGrok or X Premium\+ subscription is unclear/],
  ] as const)('shows %s its own vendor terms, not Anthropic text', (vendor, linkName, href, policy) => {
    renderWithProviders(<SubscriptionRiskDisclosure checked={false} onCheckedChange={vi.fn()} vendor={vendor} />);
    expect(screen.getByText(policy)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: linkName })).toHaveAttribute('href', href);
    expect(screen.queryByText(/Anthropic/)).toBeNull();
    expect(screen.queryByRole('link', { name: /legal and compliance/i })).toBeNull();
    expect(screen.getByRole('checkbox', { name: /read the policy and risks/i })).toBeInTheDocument();
  });

  it('shows a vendor-neutral notice with no link for an unknown vendor', () => {
    renderWithProviders(<SubscriptionRiskDisclosure checked={false} onCheckedChange={vi.fn()} vendor="other" />);
    expect(screen.getByText(/Read this vendor's terms/)).toBeInTheDocument();
    expect(screen.queryByRole('link')).toBeNull();
  });

  it('renders the one-line note with the same link', () => {
    renderWithProviders(<SubscriptionRiskNote />);
    expect(screen.getByRole('link', { name: /read the policy/i })).toHaveAttribute('href', LEGAL);
  });
});

/** Copy guard: every sentence states cited policy, never a technical block
 *  (owner decision 2026-10-16; the old "Google has blocked server-side since
 *  2026-03" sentence had no primary source and is gone). zh-TW also follows
 *  the house writing rules. */
describe('subscription risk copy', () => {
  const anthropicKeys = [
    'subscriptionRisk.policy',
    'subscriptionRisk.conflicts',
    'subscriptionRisk.enforcement',
    'subscriptionRisk.recommendation',
    'subscriptionRisk.short',
    'welcome.backend.claudeSub.risk',
    'subscriptionRisk.others',
    'subscriptionRisk.vendor.openai.policy',
    'subscriptionRisk.vendor.gemini.policy',
    'subscriptionRisk.vendor.xai.policy',
    'subscriptionRisk.vendor.other.policy',
    'subscriptionRisk.generic.conflicts',
    'subscriptionRisk.generic.recommendation',
  ];
  const catalogs: Record<string, Record<string, string>> = {
    'zh-TW': zhTW as Record<string, string>,
    en: en as Record<string, string>,
    'ja-JP': ja as Record<string, string>,
  };

  it.each(Object.keys(catalogs))('%s never claims a vendor blocks it server-side', (loc) => {
    for (const key of anthropicKeys) {
      const text = catalogs[loc][key];
      expect(text, `${loc} ${key}`).toBeTruthy();
      expect(text).not.toMatch(/封鎖|伺服器端|blocked|server side|server-side|ブロック|サーバー側|2026-03|2026 年 3 月|2026年3月|March 2026/i);
    }
  });

  it.each(Object.keys(catalogs))('%s policy copy drops "individual" (the source says ordinary use)', (loc) => {
    expect(catalogs[loc]['subscriptionRisk.policy']).not.toMatch(/individual|個人/i);
  });

  // 2026-10-16: OpenAI publishes a Sign in with ChatGPT program, so its copy
  // no longer says "unclear"; xAI was not researched and still does.
  it.each(Object.keys(catalogs))('%s cites the OpenAI program and the Gemini CLI login shutdown', (loc) => {
    const cat = catalogs[loc];
    for (const key of ['subscriptionRisk.vendor.openai.policy', 'subscriptionRisk.others']) {
      expect(cat[key], `${loc} ${key}`).toMatch(/Sign in with ChatGPT/);
      expect(cat[key], `${loc} ${key}`).not.toMatch(/OpenAI('s| and xAI's)? (position|policy)[^.]*unclear|OpenAI 對[^。]*不明|OpenAI 和 xAI|OpenAI と xAI/);
    }
    expect(cat['subscriptionRisk.vendor.openai.policy']).not.toMatch(/unclear|不明/);
    expect(cat['subscriptionRisk.vendor.xai.policy']).toMatch(/unclear|不明/);
    expect(cat['subscriptionRisk.vendor.gemini.policy']).toMatch(/Login with Google/);
    expect(cat['subscriptionRisk.vendor.gemini.policy']).toMatch(/2026/);
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
