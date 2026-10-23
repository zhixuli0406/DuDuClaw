import type { ReactNode } from 'react';
import { useIntl, type IntlShape } from 'react-intl';
import type { ExpertAgentEdits } from '@/lib/api';
import { EditsList } from './EditsList';

/** A failed `experts.install` run, as the gateway reports it. */
export interface ImportFailure {
  /** Stable code (`import.unrecognized`, `import.timeout`, …). */
  code: string;
  params?: Record<string, unknown> | null;
  /** The importer's own zh-TW text — shown only for unclassified failures. */
  text?: string;
}

/** Codes the catalogues explain; anything else renders as `import.unknown`. */
export const KNOWN_ERROR_CODES = [
  'import.unrecognized',
  'import.too_large',
  'import.too_many_entries',
  'import.zip_bomb',
  'import.not_zip',
  'import.unsafe_path',
  'import.case_duplicate',
  'import.name_encoding_ambiguous',
  'import.name_encoding_unknown',
  'import.already_imported',
  'import.replace_edited',
  'import.replace_mismatch',
  'import.replace_missing',
  'import.replace_failed',
  'import.copy_exhausted',
  'import.nothing_imported',
  'import.quota_exceeded',
  'import.premium_locked',
  'import.ai_session',
  'import.attach_under_missing',
  'import.bad_content',
  'import.source_missing',
  'import.bad_extension',
  'import.download_failed',
  'import.timeout',
  'import.spawn_failed',
  'import.unknown',
] as const;

function knownCode(code: string): string {
  return (KNOWN_ERROR_CODES as readonly string[]).includes(code) ? code : 'import.unknown';
}

/** Scalar params as strings for the message placeholders. */
function scalarValues(params: Record<string, unknown> | null | undefined): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(params ?? {})) {
    if (v === null || v === undefined || typeof v === 'object') continue;
    out[k] = String(v);
  }
  return out;
}

function formatName(intl: IntlShape, code: unknown): string {
  const key = `experts.report.format.${String(code)}`;
  return intl.messages[key] ? intl.formatMessage({ id: key }) : intl.formatMessage({ id: 'experts.report.format.pack' });
}

/** One employee's reason, rendered from the report's message code. */
function agentReason(intl: IntlShape, row: Record<string, unknown>): string {
  const code = typeof row.code === 'string' ? row.code : '';
  const key = `experts.report.msg.${code}`;
  if (code && intl.messages[key]) {
    return intl.formatMessage({ id: key }, scalarValues(row.params as Record<string, unknown>));
  }
  return intl.formatMessage({ id: 'experts.error.agents.otherReason' });
}

/**
 * ImportErrorPanel — why an AI-team import stopped, in plain language: what
 * happened, what to do about it, and the error code to quote when asking
 * for help. Code-specific extras: the archive's contents and where a
 * recognisable pack sits deeper (unrecognised layout), each employee's
 * reason (nothing imported), the edits a replace would move away. The
 * importer's own wording appears only for unclassified failures, folded.
 */
export function ImportErrorPanel({
  failure,
  children,
}: {
  failure: ImportFailure;
  /** Actions for this failure (re-import choices, confirm replace). */
  children?: ReactNode;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const code = knownCode(failure.code);
  const params = failure.params ?? {};
  const values = { ...scalarValues(params), code: failure.code };
  const entries = Array.isArray(params.entries) ? (params.entries as unknown[]).map(String) : [];
  const found = Array.isArray(params.found) ? (params.found as Array<Record<string, unknown>>) : [];
  const agents = Array.isArray(params.agents) && code === 'import.nothing_imported'
    ? (params.agents as Array<Record<string, unknown>>)
    : [];
  const edits = code === 'import.replace_edited' && Array.isArray(params.edits)
    ? (params.edits as ExpertAgentEdits[])
    : [];
  const detail = typeof params.detail === 'string' ? params.detail : failure.text;

  return (
    <div className="space-y-3 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-3 text-xs" role="alert">
      <p className="text-sm font-medium break-words">{t(`experts.error.${code}.text`, values)}</p>
      <div className="space-y-0.5">
        <p className="font-medium text-muted-foreground">{t('experts.error.fixLabel')}</p>
        <p className="break-words">{t(`experts.error.${code}.fix`, values)}</p>
      </div>

      {code === 'import.unrecognized' && (
        <div className="space-y-1.5">
          {found.length > 0 && (
            <ul className="space-y-0.5">
              {found.map((f, i) => (
                <li key={i} className="break-words">
                  {t('experts.error.found.nested', { path: String(f.path ?? ''), format: formatName(intl, f.format) })}
                </li>
              ))}
            </ul>
          )}
          <p className="font-medium text-muted-foreground">{t('experts.error.found.title')}</p>
          {entries.length > 0 ? (
            <ul className="flex flex-wrap gap-1.5">
              {entries.map((e) => (
                <li key={e} className="rounded bg-background/60 px-1.5 py-0.5 font-mono break-all">{e}</li>
              ))}
            </ul>
          ) : (
            <p>{t('experts.error.found.empty')}</p>
          )}
        </div>
      )}

      {agents.length > 0 && (
        <div className="space-y-1">
          <p className="font-medium text-muted-foreground">{t('experts.error.agents.title')}</p>
          <ul className="space-y-0.5">
            {agents.map((row, i) => (
              <li key={i} className="break-words">
                <span className="font-medium">{String(row.name ?? '')}</span>
                <span className="text-muted-foreground"> — {agentReason(intl, row)}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {edits.length > 0 && <EditsList edits={edits} />}

      {children}

      <p className="text-muted-foreground">{t('experts.error.codeLabel', { code: failure.code })}</p>
      {code === 'import.unknown' && detail && (
        <details className="text-muted-foreground">
          <summary className="cursor-pointer select-none">{t('experts.report.details')}</summary>
          <p className="mt-0.5 whitespace-pre-wrap break-words">{detail}</p>
        </details>
      )}
    </div>
  );
}
