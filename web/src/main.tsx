import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { IntlProvider } from 'react-intl';
import { BrowserRouter } from 'react-router';
import { App } from './App';
import { ToastProvider } from './components/Toast';
import { messages, useLocaleStore } from './i18n';
import { applyTheme, useThemeStore } from './stores/theme-store';
import { setTimeAgoNowLabel } from './lib/format';
import './index.css';

// Apply the persisted theme before first render. The embedded production
// server sends a strict `script-src 'self'` CSP that blocks inline scripts,
// so theme bootstrap lives here in the bundle rather than in index.html.
applyTheme(useThemeStore.getState().theme);

function Root() {
  const locale = useLocaleStore((s) => s.locale);
  // Keep the (non-hook) `timeAgo` "now" token in sync with the active locale.
  setTimeAgoNowLabel(messages[locale]?.['format.timeAgo.now'] ?? 'now');
  return (
    // IntlProvider must wrap ToastProvider: ToastItem calls useIntl(), so the
    // toast viewport has to live inside the intl context or it throws
    // "Could not find required `intl` object" the first time a toast renders.
    <IntlProvider locale={locale} messages={messages[locale]} defaultLocale="zh-TW">
      <ToastProvider>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </ToastProvider>
    </IntlProvider>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Root />
  </StrictMode>
);
