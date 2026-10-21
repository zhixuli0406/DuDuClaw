import { useIntl } from 'react-intl';
import type { ExpertImportItem, ExpertImportReport } from '@/lib/api';
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
  { id: 'skills', kinds: ['skill', 'skill-disabled', 'skill-needs', 'skill-name'] },
];

const KNOWN_FORMATS = ['doubao', 'openclaw', 'hermes', 'dify', 'workspace', 'subagent', 'instructions'];

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

/**
 * ImportReportDialog — what an AI-team upload will do (plan) or did
 * (result), in plain language: which AI staff are created, where each
 * uploaded file goes, which skills stay off until an administrator approves
 * them and why, and anything that was left out. Item text arrives already
 * human-readable from the importer; this component only groups it and
 * translates the stable `kind` / `status` codes.
 */
export function ImportReportDialog({
  open,
  phase,
  report,
  busy,
  onConfirm,
  onClose,
}: {
  open: boolean;
  phase: 'plan' | 'done';
  report: ExpertImportReport | null;
  busy: boolean;
  onConfirm: () => void;
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

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o && !busy) onClose(); }}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{phase === 'plan' ? t('experts.report.planTitle') : t('experts.report.doneTitle')}</DialogTitle>
          <DialogDescription>
            {t(phase === 'plan' ? 'experts.report.planDesc' : 'experts.report.doneDesc', { format: formatLabel })}
          </DialogDescription>
        </DialogHeader>

        {report === null ? (
          <p className="text-sm text-muted-foreground">{t('experts.report.unavailable')}</p>
        ) : (
          <div className="space-y-4">
            {disabledSkills > 0 && (
              <p className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs">
                {t('experts.report.disabledSkills', { count: disabledSkills })}
              </p>
            )}
            {[...SECTIONS.map((s) => s.id), 'notes'].map((section) => {
              const rows = items.filter((i) => sectionOf(i) === section && !(i.kind === 'format'));
              if (rows.length === 0) return null;
              return (
                <section key={section} className="space-y-1.5">
                  <h3 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    {t(`experts.report.section.${section}`)}
                  </h3>
                  <ul className="divide-y divide-surface-border rounded-lg border border-surface-border">
                    {rows.map((row, idx) => (
                      <li key={`${section}-${idx}`} className="flex items-start gap-2 px-3 py-2 text-xs">
                        <Badge variant={statusVariant(row.status)} className="shrink-0">
                          {t(`experts.report.status.${row.status}`)}
                        </Badge>
                        <div className="min-w-0 space-y-0.5">
                          <p className="break-words">
                            <span className="text-muted-foreground">{kindLabel(row.kind)}</span>
                            {row.name && <span className="ml-1.5 font-medium text-foreground">{row.name}</span>}
                          </p>
                          {row.detail && <p className="break-words text-muted-foreground">{row.detail}</p>}
                        </div>
                      </li>
                    ))}
                  </ul>
                </section>
              );
            })}
          </div>
        )}

        <DialogFooter>
          {phase === 'plan' ? (
            <>
              <Button variant="outline" size="sm" onClick={onClose} disabled={busy}>
                {t('common.cancel')}
              </Button>
              <Button variant="brand" size="sm" onClick={onConfirm} disabled={busy || report === null}>
                {busy && <Spinner className="size-3.5" />}
                {t('experts.report.confirm')}
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
