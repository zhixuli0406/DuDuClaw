import { useEffect, useState } from 'react';
import { useIntl, type IntlShape } from 'react-intl';
import type { ExpertImportItem, ExpertImportReport, ExpertReimport } from '@/lib/api';
import { AgentProfilePreview } from './AgentProfilePreview';
import { EditsList } from './EditsList';
import { ImportErrorPanel, type ImportFailure } from './ImportErrorPanel';
import { ReimportChoice } from './ReimportChoice';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Spinner,
} from '@/components/mds';

/** Report sections, in display order. Each item `kind` belongs to one. */
const SECTIONS: { id: string; kinds: string[] }[] = [
  { id: 'agents', kinds: ['agent', 'agent-model'] },
  {
    id: 'files',
    kinds: ['soul', 'identity', 'instructions', 'user-profile', 'tools-notes', 'heartbeat', 'knowledge', 'memory', 'agent-tools', 'mcp-servers'],
  },
  { id: 'skills', kinds: ['skill', 'skill-disabled', 'skill-needs', 'skill-name', 'skill-files'] },
];

const KNOWN_FORMATS = ['doubao', 'openclaw', 'hermes', 'dify', 'workspace', 'subagent', 'instructions', 'duduclaw'];

function sectionOf(item: ExpertImportItem): string {
  return SECTIONS.find((s) => s.kinds.includes(item.kind))?.id ?? 'notes';
}

function statusVariant(status: ExpertImportItem['status']) {
  switch (status) {
    case 'imported':
      return 'secondary' as const;
    case 'warning':
    case 'conflict':
      return 'destructive' as const;
    default:
      return 'ghost' as const;
  }
}

/** Translate an optional sub-code (`experts.report.<group>.<code>`), else keep it. */
function sub(intl: IntlShape, group: string, code: unknown): string {
  const key = `experts.report.${group}.${String(code)}`;
  return intl.messages[key] ? intl.formatMessage({ id: key }) : String(code ?? '');
}

/** The line in the viewer's language, or `null` when the item has no code. */
function message(intl: IntlShape, item: ExpertImportItem): string | null {
  if (!item.code) return null;
  const key = `experts.report.msg.${item.code}`;
  if (!intl.messages[key]) return null;
  const p = { ...(item.params ?? {}) } as Record<string, unknown>;
  if ('slot' in p) p.slot = sub(intl, 'kind', p.slot);
  if ('format' in p) p.format = sub(intl, 'format', p.format);
  if ('encoding' in p) p.encoding = sub(intl, 'encoding', p.encoding);
  if ('need' in p) p.need = sub(intl, 'need', p.need);
  if (Array.isArray(p.needs)) {
    p.needs = p.needs.length > 0 ? p.needs.map((n) => sub(intl, 'need', n)).join('、') : '—';
  }
  const values = Object.fromEntries(Object.entries(p).map(([k, v]) => [k, String(v ?? '')]));
  return intl.formatMessage({ id: key }, values);
}

/**
 * ImportReportDialog — what an AI-team upload will do (plan) or did
 * (result), in plain language: which AI staff are created, where each
 * uploaded file goes, which skills stay off until an administrator approves
 * them and why, and anything that was left out. Each line is rendered from
 * its message code in the viewer's language; the technical name and the
 * importer's own wording sit under a collapsed "details".
 *
 * A failed run shows why in plain language with its error code
 * ([`ImportErrorPanel`]); an upload imported before offers "replace" or
 * "import as a copy", and replacing employees edited since the import asks
 * a second time, listing what would be moved to the backup.
 */
export function ImportReportDialog({
  open,
  phase,
  report,
  failure,
  error,
  reimport,
  busy,
  onConfirm,
  onReimport,
  onClose,
}: {
  open: boolean;
  phase: 'plan' | 'done';
  report: ExpertImportReport | null;
  /** Set when the run failed (coded). */
  failure?: ImportFailure | null;
  /** Uncoded failure text (older gateways): shown as an unclassified failure. */
  error?: string;
  /** How this run treats an earlier import of the same upload. */
  reimport?: ExpertReimport | null;
  busy: boolean;
  onConfirm: (opts?: { acceptEdits?: boolean }) => void;
  /** Re-run the plan with a re-import choice. */
  onReimport?: (choice: ExpertReimport) => void;
  onClose: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const items = report?.items ?? [];
  const format = report?.meta.format;
  const formatLabel =
    format && KNOWN_FORMATS.includes(format) ? t(`experts.report.format.${format}`) : t('experts.report.format.pack');
  const disabledSkills = Number(report?.meta.disabled_skills ?? 0);
  const kindLabel = (kind: string) =>
    intl.messages[`experts.report.kind.${kind}`] ? t(`experts.report.kind.${kind}`) : t('experts.report.kind.other');
  const shownFailure: ImportFailure | null =
    failure ?? (error ? { code: report?.error?.code ?? 'import.unknown', params: report?.error?.params, text: error } : null);
  const failed = shownFailure !== null;
  const replaceInfo = report?.details?.replace ?? null;
  const needsEditConsent = phase === 'plan' && !failed && reimport?.mode === 'replace' && Boolean(replaceInfo?.edited);
  const [confirmingEdits, setConfirmingEdits] = useState(false);
  useEffect(() => {
    if (!open) setConfirmingEdits(false);
  }, [open]);
  const failureCode = shownFailure?.code;
  const hideItems = failureCode === 'import.already_imported' || confirmingEdits;
  const conflictOnly =
    failureCode === 'import.nothing_imported' &&
    Array.isArray(shownFailure?.params?.agents) &&
    (shownFailure?.params?.agents as Array<Record<string, unknown>>).some((a) => a.code === 'agent.conflict');

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o && !busy) onClose(); }}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            {failureCode === 'import.already_imported'
              ? t('experts.reimport.title')
              : failed
                ? t('experts.report.failedTitle')
                : phase === 'plan'
                  ? t('experts.report.planTitle')
                  : t('experts.report.doneTitle')}
          </DialogTitle>
          <DialogDescription>
            {failureCode === 'import.already_imported'
              ? t('experts.reimport.desc')
              : failed
              ? t('experts.report.failedDesc')
              : t(phase === 'plan' ? 'experts.report.planDesc' : 'experts.report.doneDesc', { format: formatLabel })}
          </DialogDescription>
        </DialogHeader>

        {phase === 'plan' && !failed && report?.details?.profile_preview && (
          <AgentProfilePreview profiles={report.details.profile_preview} />
        )}

        {shownFailure && (
          <ImportErrorPanel failure={shownFailure}>
            {failureCode === 'import.already_imported' && onReimport && (
              <ReimportChoice
                slug={String(shownFailure.params?.slug ?? '')}
                edited={Boolean(shownFailure.params?.edited)}
                busy={busy}
                onChoose={onReimport}
              />
            )}
            {conflictOnly && onReimport && <ReimportChoice busy={busy} onChoose={onReimport} />}
            {failureCode === 'import.replace_edited' && (
              <Button variant="destructive" size="sm" disabled={busy} onClick={() => onConfirm({ acceptEdits: true })}>
                {busy && <Spinner className="size-3.5" />}
                {t('experts.reimport.edits.confirm')}
              </Button>
            )}
          </ImportErrorPanel>
        )}

        {!failed && phase === 'plan' && reimport?.mode === 'replace' && replaceInfo && (
          <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs">
            {t('experts.reimport.planReplace', { name: replaceInfo.name, count: replaceInfo.agents.length })}
          </p>
        )}
        {!failed && phase === 'plan' && reimport?.mode === 'copy' && (
          <p className="rounded-lg border border-surface-border px-3 py-2 text-xs">{t('experts.reimport.planCopy')}</p>
        )}

        {confirmingEdits && replaceInfo && (
          <div className="space-y-2 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-3 text-xs">
            <p className="text-sm font-medium">{t('experts.reimport.edits.title')}</p>
            <p>{t('experts.reimport.edits.desc')}</p>
            <EditsList edits={replaceInfo.edits} />
          </div>
        )}

        {hideItems ? null : report === null ? (
          !failed && <p className="text-sm text-muted-foreground">{t('experts.report.unavailable')}</p>
        ) : (
          <div className="space-y-4">
            {disabledSkills > 0 && !failed && (
              <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs">
                {t('experts.report.disabledSkills', { count: disabledSkills })}
              </p>
            )}
            {[...SECTIONS.map((s) => s.id), 'notes'].map((section) => {
              const rows = items.filter((i) => sectionOf(i) === section && i.kind !== 'format');
              if (rows.length === 0) return null;
              return (
                <section key={section} className="space-y-1.5">
                  <h3 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    {t(`experts.report.section.${section}`)}
                  </h3>
                  <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
                    {rows.map((row, idx) => {
                      const text = message(intl, row);
                      const technical = [row.name, row.detail].filter(Boolean).join(' — ');
                      return (
                        <li key={`${section}-${idx}`} className="flex items-start gap-2 px-3 py-2 text-xs">
                          <Badge variant={statusVariant(row.status)} className="shrink-0">
                            {t(`experts.report.status.${row.status}`)}
                          </Badge>
                          <div className="min-w-0 space-y-0.5">
                            <p className="break-words">
                              <span className="text-muted-foreground">{kindLabel(row.kind)}</span>
                              <span className="ml-1.5 text-foreground">{text ?? (row.detail || row.name)}</span>
                            </p>
                            {text && technical && (
                              <details className="text-muted-foreground">
                                <summary className="cursor-pointer select-none">{t('experts.report.details')}</summary>
                                <p className="mt-0.5 break-words">{technical}</p>
                              </details>
                            )}
                          </div>
                        </li>
                      );
                    })}
                  </ul>
                </section>
              );
            })}
          </div>
        )}

        <DialogFooter>
          {confirmingEdits ? (
            <>
              <Button variant="outline" size="sm" onClick={() => setConfirmingEdits(false)} disabled={busy}>
                {t('experts.reimport.edits.back')}
              </Button>
              <Button variant="destructive" size="sm" onClick={() => onConfirm({ acceptEdits: true })} disabled={busy}>
                {busy && <Spinner className="size-3.5" />}
                {t('experts.reimport.edits.confirm')}
              </Button>
            </>
          ) : phase === 'plan' && !failed ? (
            <>
              <Button variant="outline" size="sm" onClick={onClose} disabled={busy}>
                {t('common.cancel')}
              </Button>
              <Button
                variant="brand"
                size="sm"
                onClick={() => (needsEditConsent ? setConfirmingEdits(true) : onConfirm())}
                disabled={busy || report === null || (!!report.details?.profile_preview && !report.details.profile_confirmation)}
              >
                {busy && <Spinner className="size-3.5" />}
                {reimport?.mode === 'replace' ? t('experts.reimport.confirmReplace') : t('experts.report.confirm')}
              </Button>
            </>
          ) : (
            <Button variant="brand" size="sm" onClick={onClose}>
              {t('experts.report.close')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
