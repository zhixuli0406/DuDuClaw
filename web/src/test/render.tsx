import { type ReactElement } from 'react';
import { render, type RenderOptions } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { IntlProvider } from 'react-intl';
import en from '@/i18n/en.json';

function AllProviders({ children }: { children: React.ReactNode }) {
  return (
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter>{children}</MemoryRouter>
    </IntlProvider>
  );
}

export function renderWithProviders(
  ui: ReactElement,
  options?: Omit<RenderOptions, 'wrapper'>
) {
  return render(ui, { wrapper: AllProviders, ...options });
}

export { render };
