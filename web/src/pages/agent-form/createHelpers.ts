import type { IntlShape } from 'react-intl';
import { sanitizeErrorDetail } from '@/lib/error-message';

/** Shared required-name checks for the full form and member-avatar dialog. */
export function agentCreateNamesPresent(name: string, displayName: string): boolean {
  return !!name.trim() && !!displayName.trim();
}

export function agentCreateErrorMessage(cause: unknown, intl: IntlShape): string {
  const detail = sanitizeErrorDetail(cause);
  if (/^Agent name must be lowercase alphanumeric with hyphens, max 64 chars$/i.test(detail)) {
    return intl.formatMessage({ id: 'agents.create.error' });
  }
  if (/^Agent '.+' already exists$/i.test(detail) || /^已有同名的 AI 員工\(.+\),請換一個名稱$/.test(detail)) {
    return intl.formatMessage({ id: 'agents.create.errorExists' });
  }
  return intl.formatMessage(
    { id: 'agents.create.errorServer' },
    { detail: detail || intl.formatMessage({ id: 'common.error' }) },
  );
}
