import { Navigate, useSearchParams } from 'react-router';
import { BillingPage } from './BillingPage';

/**
 * BillingShell — `/manage/billing`. Until 2026-08-04 this was a two-tab shell
 * (帳務 + 帳號輪換); the accounts tab became its own management entry
 * (`/manage/accounts`, D16/D18) because one-click CLI sign-in was impossible to
 * find nested two levels down. What is left is the billing page itself, plus a
 * redirect so older `?tab=accounts` links (bookmarks, docs, the world map) land
 * on the new page instead of a tab that no longer exists.
 */
export function BillingShell() {
  const [params] = useSearchParams();
  if (params.get('tab') === 'accounts') return <Navigate to="/manage/accounts" replace />;
  return <BillingPage />;
}
