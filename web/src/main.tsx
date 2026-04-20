import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { IntlProvider } from 'react-intl';
import { BrowserRouter } from 'react-router';
import { App } from './App';
import { ToastProvider } from './components/Toast';
import { messages, useLocaleStore } from './i18n';
import './index.css';

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
