import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { IntlProvider } from 'react-intl';
import { BrowserRouter } from 'react-router';
import { App } from './App';
import { ToastProvider } from './components/Toast';
import { messages, useLocaleStore } from './i18n';
import { applyTheme, useThemeStore } from './stores/theme-store';
import './index.css';

// Apply the persisted theme before first render. The embedded production
// server sends a strict `script-src 'self'` CSP that blocks inline scripts,
// so theme bootstrap lives here in the bundle rather than in index.html.
applyTheme(useThemeStore.getState().theme);

function Root() {
  const locale = useLocaleStore((s) => s.locale);
  return (
    <ToastProvider>
      <IntlProvider locale={locale} messages={messages[locale]} defaultLocale="zh-TW">
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </IntlProvider>
    </ToastProvider>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Root />
  </StrictMode>
);
