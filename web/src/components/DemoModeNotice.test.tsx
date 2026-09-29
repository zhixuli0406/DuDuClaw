import { beforeEach, describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';
import zh from '@/i18n/zh-TW.json';
import ja from '@/i18n/ja-JP.json';
import { DemoModeNotice, type DemoDataOrigin } from './DemoModeNotice';

function renderNotice(props: { pageKey: string; origin?: DemoDataOrigin }) {
  return render(
    <IntlProvider locale="en" messages={en}>
      <DemoModeNotice {...props} />
    </IntlProvider>,
  );
}

describe('DemoModeNotice', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('states the synthetic-data limitation by default', () => {
    renderNotice({ pageKey: 'decision-lab' });
    expect(screen.getByTestId('demo-mode-notice')).toHaveTextContent(en['demoMode.synthetic']);
  });

  it('switches wording when the backend reports operator-supplied data', () => {
    renderNotice({ pageKey: 'decision-lab', origin: 'operator' });
    expect(screen.getByTestId('demo-mode-notice')).toHaveTextContent(en['demoMode.operator']);
  });

  it('names the task board as a real, non-synthetic source', () => {
    renderNotice({ pageKey: 'decision-lab', origin: 'taskBoard' });
    const notice = screen.getByTestId('demo-mode-notice');
    expect(notice).toHaveTextContent(en['demoMode.taskBoard']);
    // "Real" must never read as "validated": the proxy caveat travels with it.
    expect(notice.textContent).toMatch(/proxy/i);
  });

  it('names the audit trail as a real source while keeping the co-occurrence caveat', () => {
    renderNotice({ pageKey: 'causal-curation', origin: 'auditTrail' });
    const notice = screen.getByTestId('demo-mode-notice');
    expect(notice).toHaveTextContent(en['demoMode.auditTrail']);
    expect(notice.textContent).toMatch(/not inferred causal effects/i);
  });

  it('dismisses and stays dismissed for that page only', async () => {
    const user = userEvent.setup();
    const { unmount } = renderNotice({ pageKey: 'decision-lab' });
    await user.click(screen.getByRole('button', { name: en['demoMode.dismiss'] }));
    expect(screen.queryByTestId('demo-mode-notice')).toBeNull();
    unmount();

    // Same page: still dismissed after a remount.
    renderNotice({ pageKey: 'decision-lab' });
    expect(screen.queryByTestId('demo-mode-notice')).toBeNull();

    // A different page keeps its own notice.
    renderNotice({ pageKey: 'ccr-dashboard' });
    expect(screen.getByTestId('demo-mode-notice')).toBeInTheDocument();
  });

  it('renders when localStorage is unavailable instead of throwing', () => {
    const original = Object.getOwnPropertyDescriptor(window, 'localStorage');
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      get() {
        throw new Error('site data blocked');
      },
    });
    try {
      renderNotice({ pageKey: 'decision-lab' });
      expect(screen.getByTestId('demo-mode-notice')).toBeInTheDocument();
    } finally {
      if (original) Object.defineProperty(window, 'localStorage', original);
    }
  });

  it('has all four demo-mode strings in every shipped locale', () => {
    const keys = ['demoMode.synthetic', 'demoMode.operator', 'demoMode.dismiss'] as const;
    for (const bundle of [en, zh, ja] as Array<Record<string, string>>) {
      for (const key of keys) {
        expect(bundle[key], `${key} missing`).toBeTruthy();
      }
    }
  });
});
