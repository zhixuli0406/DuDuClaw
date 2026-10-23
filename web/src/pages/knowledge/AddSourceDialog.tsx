import { useEffect, useRef, useState, type DragEvent } from 'react';
import { useIntl } from 'react-intl';
import { CheckCircle2, Copy, FileUp, X, XCircle } from 'lucide-react';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Segmented,
  Textarea,
} from '@/components/mds';
import { toast, formatError } from '@/lib/toast';
import {
  KB_MAX_FILES_PER_UPLOAD,
  KB_UPLOAD_ACCEPT,
  knowledgeApi,
  precheckFile,
  uploadSources,
  type KbCollection,
  type KbLimits,
  type KbUploadFileResult,
} from '@/lib/knowledge-api';
import { formatBytes } from './labels';

type Mode = 'file' | 'paste' | 'url';

/** http(s) only; the gateway does the real safety checks when it fetches. */
export function looksLikeWebAddress(raw: string): boolean {
  try {
    const u = new URL(raw.trim());
    return (u.protocol === 'http:' || u.protocol === 'https:') && u.hostname !== '';
  } catch {
    return false;
  }
}

/**
 * 新增來源 — upload files (drag and drop or choose), paste text, or add a
 * web page. Files show a progress bar while they are sent and then one
 * line per file: added, already in this collection, or why it was refused.
 */
export function AddSourceDialog({
  collection,
  onClose,
  onAdded,
}: {
  collection: KbCollection;
  onClose: () => void;
  onAdded: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [mode, setMode] = useState<Mode>('file');
  const [files, setFiles] = useState<File[]>([]);
  const [dragging, setDragging] = useState(false);
  const [progress, setProgress] = useState<number | null>(null);
  const [results, setResults] = useState<KbUploadFileResult[] | null>(null);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [title, setTitle] = useState('');
  const [text, setText] = useState('');
  const [url, setUrl] = useState('');
  const [busy, setBusy] = useState(false);
  const [limits, setLimits] = useState<KbLimits | null>(null);
  const [skipped, setSkipped] = useState<Array<{ name: string; reason: 'type' | 'size' | 'count' }>>([]);
  const inputRef = useRef<HTMLInputElement>(null);

  // The gateway's own limits, so a file it would refuse is not sent at all.
  // Without them (older gateway, error) every file is sent and the gateway
  // still decides.
  useEffect(() => {
    let alive = true;
    knowledgeApi
      .limits()
      .then((l) => alive && setLimits(l))
      .catch(() => {
        /* Fall back to sending and letting the gateway refuse. */
      });
    return () => {
      alive = false;
    };
  }, []);
  const maxFiles = limits?.max_files_per_upload ?? KB_MAX_FILES_PER_UPLOAD;
  const accept = limits ? limits.extensions.map((e) => `.${e}`).join(',') : KB_UPLOAD_ACCEPT;
  const abortRef = useRef<(() => void) | null>(null);

  const addFiles = (list: FileList | null) => {
    if (!list) return;
    setResults(null);
    setUploadError(null);
    const refused: Array<{ name: string; reason: 'type' | 'size' | 'count' }> = [];
    const merged = [...files];
    for (const f of Array.from(list)) {
      const reason = precheckFile(f, limits);
      if (reason) refused.push({ name: f.name, reason });
      else if (merged.some((m) => m.name === f.name && m.size === f.size)) continue;
      else if (merged.length >= maxFiles) refused.push({ name: f.name, reason: 'count' });
      else merged.push(f);
    }
    setFiles(merged);
    setSkipped(refused);
  };

  const onDrop = (e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDragging(false);
    addFiles(e.dataTransfer?.files ?? null);
  };

  const upload = async () => {
    if (files.length === 0 || busy) return;
    setBusy(true);
    setUploadError(null);
    setProgress(0);
    const handle = uploadSources(collection.id, files, setProgress);
    abortRef.current = handle.abort;
    try {
      const r = await handle.promise;
      setResults(r);
      setFiles([]);
      if (r.some((x) => x.ok)) onAdded();
    } catch (e) {
      setUploadError(formatError(e));
    } finally {
      abortRef.current = null;
      setBusy(false);
      setProgress(null);
    }
  };

  const submitText = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const r =
        mode === 'paste'
          ? await knowledgeApi.addPaste(collection.id, text, title.trim() || undefined)
          : await knowledgeApi.addUrl(collection.id, url.trim(), title.trim() || undefined);
      toast.success(r.duplicate ? t('kb.add.duplicate', { title: r.source.title }) : t('kb.add.queued', { title: r.source.title }));
      onAdded();
      onClose();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
    }
  };

  const close = () => {
    abortRef.current?.();
    onClose();
  };

  const modeOptions = [
    { value: 'file' as const, label: t('kb.add.mode.file') },
    { value: 'paste' as const, label: t('kb.add.mode.paste') },
    { value: 'url' as const, label: t('kb.add.mode.url') },
  ];
  const urlOk = looksLikeWebAddress(url);
  const pct = progress === null ? 0 : Math.round(progress * 100);

  return (
    <Dialog open onOpenChange={(o) => !o && close()}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t('kb.source.add')}</DialogTitle>
          <DialogDescription>{t('kb.add.desc', { name: collection.name })}</DialogDescription>
        </DialogHeader>
        <Segmented aria-label={t('kb.add.modeLabel')} value={mode} onValueChange={setMode} options={modeOptions} />

        {/* One body box for all three modes: the dialog is centred, so a body that changed height moved it. */}
        <div className="min-h-[19rem]" data-testid="add-source-body">
        {mode === 'file' && (
          <div className="space-y-3">
            <div
              onDragOver={(e) => {
                e.preventDefault();
                setDragging(true);
              }}
              onDragLeave={() => setDragging(false)}
              onDrop={onDrop}
              data-testid="kb-dropzone"
              className={
                'flex flex-col items-center gap-2 rounded-xl border border-dashed px-4 py-6 text-center transition-colors ' +
                (dragging ? 'border-brand bg-brand/7' : 'border-surface-border')
              }
            >
              <FileUp aria-hidden className="size-6 text-muted-foreground" />
              <p className="text-sm">{t('kb.add.drop')}</p>
              <Button variant="outline" size="sm" onClick={() => inputRef.current?.click()} disabled={busy}>
                {t('kb.add.choose')}
              </Button>
              <input
                ref={inputRef}
                type="file"
                multiple
                accept={accept}
                className="sr-only"
                tabIndex={-1}
                aria-label={t('kb.add.choose')}
                onChange={(e) => {
                  addFiles(e.target.files);
                  e.target.value = '';
                }}
              />
              <p className="text-xs text-muted-foreground">{t('kb.add.formats', { max: maxFiles })}</p>
              {limits && (
                <p className="text-xs text-muted-foreground">
                  {t('kb.add.sizeLimit', { mb: Math.floor(limits.max_source_bytes / (1024 * 1024)) })}
                </p>
              )}
            </div>

            {files.length > 0 && (
              <ul className="max-h-40 divide-y divide-surface-border overflow-y-auto rounded-lg border border-surface-border">
                {files.map((f) => (
                  <li key={`${f.name}-${f.size}`} className="flex items-center gap-2 px-3 py-1.5 text-sm">
                    <span className="min-w-0 flex-1 truncate" title={f.name}>
                      {f.name}
                    </span>
                    <span className="font-mono text-xs tabular-nums text-muted-foreground">{formatBytes(f.size)}</span>
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      disabled={busy}
                      aria-label={t('kb.add.removeFile', { name: f.name })}
                      onClick={() => setFiles((prev) => prev.filter((x) => x !== f))}
                    >
                      <X />
                    </Button>
                  </li>
                ))}
              </ul>
            )}

            {skipped.length > 0 && (
              <ul role="alert" className="space-y-1 text-sm text-destructive" aria-label={t('kb.add.skipped')}>
                {skipped.map((x, i) => (
                  <li key={`${x.name}-${i}`} className="break-all">
                    {x.name} — {t(`kb.add.skip.${x.reason}`, {
                      max: maxFiles,
                      mb: limits ? Math.floor(limits.max_source_bytes / (1024 * 1024)) : 0,
                    })}
                  </li>
                ))}
              </ul>
            )}

            {progress !== null && (
              <div className="space-y-1">
                <div
                  role="progressbar"
                  aria-label={t('kb.add.uploading')}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={pct}
                  className="h-2 overflow-hidden rounded-full bg-muted"
                >
                  <div className="h-full bg-brand transition-[width] duration-200" style={{ width: `${pct}%` }} />
                </div>
                <p className="text-xs text-muted-foreground">{t('kb.add.uploadingPct', { pct })}</p>
              </div>
            )}

            {uploadError && (
              <p role="alert" className="text-sm text-destructive">
                {uploadError}
              </p>
            )}

            {results && (
              <ul className="space-y-1.5" aria-label={t('kb.add.results')}>
                {results.map((r, i) => (
                  <li key={`${r.name}-${i}`} className="flex items-start gap-2 text-sm">
                    {r.ok && r.duplicate ? (
                      <Copy aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                    ) : r.ok ? (
                      <CheckCircle2 aria-hidden className="mt-0.5 size-4 shrink-0 text-emerald-700 dark:text-emerald-400" />
                    ) : (
                      <XCircle aria-hidden className="mt-0.5 size-4 shrink-0 text-destructive" />
                    )}
                    <span className="min-w-0">
                      <span className="font-medium break-all">{r.name}</span>
                      {' — '}
                      {r.ok && r.duplicate
                        ? t('kb.add.result.duplicate')
                        : r.ok
                          ? t('kb.add.result.queued')
                          : (r.message ?? t('kb.add.result.failed'))}
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        {mode !== 'file' && (
          <div className="space-y-3">
            <div className="space-y-1.5">
              <label htmlFor="kb-add-title" className="text-sm font-medium">
                {t('kb.add.title')}
              </label>
              <Input id="kb-add-title" value={title} maxLength={120} onChange={(e) => setTitle(e.target.value)} />
            </div>
            {mode === 'paste' ? (
              <div className="space-y-1.5">
                <label htmlFor="kb-add-text" className="text-sm font-medium">
                  {t('kb.add.text')}
                </label>
                <Textarea id="kb-add-text" rows={8} value={text} onChange={(e) => setText(e.target.value)} />
              </div>
            ) : (
              <div className="space-y-1.5">
                <label htmlFor="kb-add-url" className="text-sm font-medium">
                  {t('kb.add.url')}
                </label>
                <Input
                  id="kb-add-url"
                  type="url"
                  inputMode="url"
                  value={url}
                  placeholder="https://"
                  aria-invalid={url.trim() !== '' && !urlOk}
                  onChange={(e) => setUrl(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">{t('kb.add.urlHint')}</p>
              </div>
            )}
          </div>
        )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={close}>
            {results ? t('common.close') : t('common.cancel')}
          </Button>
          {mode === 'file' ? (
            <Button variant="brand" onClick={() => void upload()} disabled={files.length === 0 || busy}>
              {t('kb.add.upload', { count: files.length })}
            </Button>
          ) : (
            <Button
              variant="brand"
              onClick={() => void submitText()}
              disabled={busy || (mode === 'paste' ? text.trim() === '' : !urlOk)}
            >
              {t('common.add')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
