import { useEffect, useState, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { Check, Loader2, AlertTriangle, UploadCloud } from 'lucide-react';
import { cn } from '@/lib/utils';
import { api, type RedactionProfileImportResult } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  Button,
  Input,
  Textarea,
  Badge,
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/mds';
import { FieldBlock } from '@/pages/agent-form/form-rows';
import { categoryLabel } from './RedactionTab';

// ── 匯入規則包 dialog (canvas screen 9, §12.2 拍板 C) ─────────────────────
//
// §12.3: this is the ONE place in the "我的規則" surface allowed to say
// ".toml" — everywhere else on the main path stays silent about the storage
// format. Auto-previews (debounced) via `redaction.profiles.import` with
// `dry_run: true` as the operator pastes/drops content, then commits with a
// second call omitting `dry_run`.

function ErrorBanner({ message }: { message: string }) {
  return (
    <div className="flex gap-2.5 rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2.5 text-xs text-destructive">
      <AlertTriangle className="mt-0.5 size-4 shrink-0" />
      <span>{message}</span>
    </div>
  );
}

export function RedactionImportProfileDialog({
  open,
  onClose,
  onImported,
  categoryLabels,
}: {
  open: boolean;
  onClose: () => void;
  /** Called after a successful (non-dry-run) import — the caller reloads
   *  `redaction.get` so the new profile shows up ticked in the 偵測規則集
   *  list. */
  onImported: () => void;
  /** Best-effort display names for categories already known to this
   *  gateway. A freshly-imported pack's OWN categories will still render as
   *  raw ids in the preview (their `[meta.labels]` mapping isn't persisted
   *  — and therefore not reflected in `RedactionConfig.category_labels` —
   *  until the import actually commits), which is acceptable here per
   *  §12.3's advanced/import carve-out. */
  categoryLabels?: Record<string, string>;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [content, setContent] = useState('');
  const [name, setName] = useState('');
  const [dragOver, setDragOver] = useState(false);

  const [preview, setPreview] = useState<RedactionProfileImportResult | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);

  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setContent('');
    setName('');
    setDragOver(false);
    setPreview(null);
    setPreviewError(null);
    setImportError(null);
  }, [open]);

  // Debounced auto-preview — the canvas has no explicit "預覽" button, the
  // preview table just appears once there's something to check.
  useEffect(() => {
    if (!open) return;
    if (!content.trim()) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    const timer = setTimeout(() => {
      setPreviewing(true);
      setPreviewError(null);
      api.redaction.profiles
        .import(content, name.trim() || undefined, true)
        .then((result) => setPreview(result))
        .catch((e) => {
          setPreview(null);
          setPreviewError(formatError(e));
        })
        .finally(() => setPreviewing(false));
    }, 500);
    return () => clearTimeout(timer);
  }, [content, name, open]);

  const handleDrop = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDragOver(false);
    const file = e.dataTransfer.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === 'string') setContent(reader.result);
    };
    reader.readAsText(file);
  };

  const handleImport = async () => {
    if (!preview || preview.imported === 0) return;
    setImporting(true);
    setImportError(null);
    try {
      const result = await api.redaction.profiles.import(content, name.trim() || undefined, false);
      // Written to `<slug>.toml` but not applied live — keep the dialog open
      // and say so rather than reporting a plain success.
      if (result.warning) {
        setImportError(result.warning);
        return;
      }
      toast.success(t('redaction.import.success', { name: result.name }));
      onImported();
    } catch (e) {
      setImportError(formatError(e));
    } finally {
      setImporting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t('redaction.import.title')}</DialogTitle>
        </DialogHeader>

        <div className="max-h-[65vh] space-y-3.5 overflow-y-auto py-1">
          <div
            onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
            onDragLeave={() => setDragOver(false)}
            onDrop={handleDrop}
            className={cn(
              'flex flex-col items-center gap-1 rounded-xl border-2 border-dashed px-5 py-6 text-center text-xs',
              dragOver ? 'border-brand bg-brand/10' : 'border-brand/30 bg-brand/5',
            )}
          >
            <UploadCloud className="mb-1 size-5 text-brand" />
            <p className="font-medium text-foreground">{t('redaction.import.drop.title')}</p>
            <p className="text-muted-foreground">{t('redaction.import.drop.hint')}</p>
          </div>

          <FieldBlock label={t('redaction.import.paste.label')}>
            <Textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              className="min-h-40 font-mono text-xs"
              placeholder={t('redaction.import.paste.placeholder') as string}
            />
          </FieldBlock>

          <FieldBlock label={t('redaction.import.name.label')} description={t('redaction.import.name.hint')}>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t('redaction.import.name.placeholder') as string} />
          </FieldBlock>

          {previewing && <p className="text-xs text-muted-foreground">{t('redaction.import.previewing')}</p>}
          {previewError && <ErrorBanner message={t('redaction.import.previewError', { message: previewError }) as string} />}

          {preview && (
            <FieldBlock label={t('redaction.import.preview.label')}>
              <div className="overflow-x-auto rounded-lg border border-surface-border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t('redaction.import.preview.table.name')}</TableHead>
                      <TableHead>{t('redaction.import.preview.table.ruleCount')}</TableHead>
                      <TableHead>{t('redaction.import.preview.table.categories')}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    <TableRow>
                      <TableCell className="font-medium text-foreground">{preview.name}</TableCell>
                      <TableCell>{preview.imported}</TableCell>
                      <TableCell>
                        <div className="flex flex-wrap gap-1">
                          {preview.categories.map((c) => (
                            <Badge key={c} variant="secondary">{categoryLabel(intl, c, categoryLabels)}</Badge>
                          ))}
                        </div>
                      </TableCell>
                    </TableRow>
                  </TableBody>
                </Table>
              </div>

              {preview.skipped.length > 0 && (
                <div className="mt-2 flex gap-2.5 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2.5 text-xs">
                  <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
                  <div className="space-y-1 text-muted-foreground">
                    {preview.skipped.map((s, i) => (
                      <p key={i}>
                        {s.line != null
                          ? t('redaction.import.preview.skippedLine', { rule: s.rule_id, line: s.line, reason: s.reason })
                          : t('redaction.import.preview.skipped', { rule: s.rule_id, reason: s.reason })}
                      </p>
                    ))}
                  </div>
                </div>
              )}

              {preview.imported === 0 && (
                <p className="mt-2 text-xs text-destructive">{t('redaction.import.preview.zero')}</p>
              )}
            </FieldBlock>
          )}

          {importError && <ErrorBanner message={t('redaction.import.error', { message: importError }) as string} />}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t('common.cancel')}</Button>
          <Button variant="brand" onClick={() => void handleImport()} disabled={importing || !preview || preview.imported === 0}>
            {importing ? <Loader2 className="animate-spin" /> : <Check />}
            {t('redaction.import.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
