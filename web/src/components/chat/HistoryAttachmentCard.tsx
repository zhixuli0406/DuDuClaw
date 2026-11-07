import { useEffect, useRef, useState } from 'react';
import { Download, FileText, Loader2 } from 'lucide-react';
import { useIntl } from 'react-intl';
import { useAuthStore } from '@/stores/auth-store';
import { HISTORY_DOWNLOAD_MAX_BYTES, HISTORY_PREVIEW_MAX_BYTES, historyDownloadName, isHistoryImage, readHistoryAttachmentBlob, type HistoryAttachment } from '@/lib/history-attachments';

/** Auth changes discard both in-flight requests and already fetched previews. */
export function HistoryAttachmentCard({ attachment }: { attachment: HistoryAttachment }) {
  const jwt = useAuthStore(s => s.jwt);
  return <SessionAttachmentCard key={`${jwt ?? ''}:${attachment.id}`} attachment={attachment} jwt={jwt} />;
}

function SessionAttachmentCard({ attachment, jwt }: { attachment: HistoryAttachment; jwt: string | null }) {
  const intl = useIntl();
  const [preview, setPreview] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const lifetime = useRef<{ controller: AbortController; urls: Set<string>; timers: Set<number> } | null>(null);
  const card = useRef<HTMLDivElement>(null);
  const image = isHistoryImage(attachment.mime);
  const canPreview = image && attachment.size <= HISTORY_PREVIEW_MAX_BYTES;
  const canDownload = attachment.size <= HISTORY_DOWNLOAD_MAX_BYTES;
  const msg = (key: string) => intl.formatMessage({ id: `webchat.attachment.${key}` });

  useEffect(() => {
    const scope = { controller: new AbortController(), urls: new Set<string>(), timers: new Set<number>() };
    lifetime.current = scope;
    setPreview(null);
    setFailed(false);
    let observer: IntersectionObserver | undefined;
    if (canPreview && jwt) {
      let started = false;
      const loadPreview = () => {
        if (started || scope.controller.signal.aborted) return;
        started = true;
        observer?.disconnect();
        void fetchBlob(scope.controller.signal, true).then(({ blob }) => {
          if (scope.controller.signal.aborted) return;
          // Do not trust an unexpected response type as an image preview.
          if (blob.type !== attachment.mime) throw new Error('unexpected attachment type');
          const url = URL.createObjectURL(blob);
          scope.urls.add(url);
          setPreview(url);
        }).catch(() => { if (!scope.controller.signal.aborted) setFailed(true); });
      };
      // loading="lazy" on an <img> cannot defer the fetch that creates its blob.
      // Start that request only when the card approaches the scroll viewport.
      if (typeof IntersectionObserver !== 'undefined' && card.current) {
        observer = new IntersectionObserver(entries => {
          if (entries.some(entry => entry.isIntersecting)) loadPreview();
        }, { rootMargin: '300px' });
        observer.observe(card.current);
      } else {
        loadPreview();
      }
    }
    return () => {
      observer?.disconnect();
      scope.controller.abort();
      for (const url of scope.urls) URL.revokeObjectURL(url);
      for (const timer of scope.timers) window.clearTimeout(timer);
      if (lifetime.current === scope) lifetime.current = null;
    };
    // The wrapper remounts on JWT/id changes; MIME may change on history refresh.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [jwt, attachment.id, attachment.mime, attachment.size, canPreview]);

  async function fetchBlob(signal: AbortSignal, preview = false): Promise<{ blob: Blob; filename: string }> {
    if (!jwt) throw new Error('authentication required');
    const query = new URLSearchParams({ id: attachment.id });
    if (preview) query.set('preview', 'true');
    const response = await fetch(`/api/chat/attachments?${query}`, {
      headers: { Authorization: `Bearer ${jwt}` },
      cache: 'no-store',
      signal,
    });
    if (!response.ok) throw new Error('attachment unavailable');
    const blob = await readHistoryAttachmentBlob(response, preview ? HISTORY_PREVIEW_MAX_BYTES : HISTORY_DOWNLOAD_MAX_BYTES, signal);
    if (useAuthStore.getState().jwt !== jwt || signal.aborted) throw new Error('session changed');
    return { blob, filename: historyDownloadName(response.headers?.get('content-disposition') ?? null, attachment.name) };
  }

  async function download() {
    const scope = lifetime.current;
    if (!scope || busy || !canDownload) return;
    setBusy(true);
    setFailed(false);
    try {
      // Re-authorize on each click even when a thumbnail was fetched earlier.
      const { blob, filename } = await fetchBlob(scope.controller.signal);
      if (scope.controller.signal.aborted) return;
      const url = URL.createObjectURL(blob);
      scope.urls.add(url);
      const link = document.createElement('a');
      link.href = url;
      link.download = filename;
      document.body.appendChild(link);
      link.click();
      link.remove();
      // Leave time for the browser to consume the URL, but never retain file
      // buffers indefinitely while a conversation stays open.
      const timer = window.setTimeout(() => {
        if (scope.urls.delete(url)) URL.revokeObjectURL(url);
        scope.timers.delete(timer);
      }, 30_000);
      scope.timers.add(timer);
    } catch {
      if (!scope.controller.signal.aborted) setFailed(true);
    } finally {
      if (!scope.controller.signal.aborted) setBusy(false);
    }
  }

  return <div ref={card} className="flex w-64 max-w-full flex-col gap-2 rounded-lg border border-surface-border bg-surface p-3 text-foreground">
    {preview && <img src={preview} alt={attachment.name} className="max-h-40 w-full rounded object-contain" loading="lazy" onError={() => { setPreview(null); setFailed(true); }} />}
    <div className="flex min-w-0 items-start gap-2">
      <FileText className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <div className="min-w-0">
        <div className="break-words text-sm font-medium">{attachment.name}</div>
        <div className="break-all text-xs text-muted-foreground">{attachment.mime || 'application/octet-stream'} · {intl.formatNumber(attachment.size, { style: 'unit', unit: 'byte', notation: 'compact' })}</div>
      </div>
    </div>
    {!image && <p className="text-xs text-muted-foreground">{msg('downloadToOpen')}</p>}
    {image && !canPreview && canDownload && <p className="text-xs text-muted-foreground">{msg('previewTooLarge')}</p>}
    {!canDownload && <p className="text-xs text-muted-foreground">{msg('tooLarge')}</p>}
    {failed && <p role="alert" className="text-xs text-destructive">{msg('unavailable')}</p>}
    <button type="button" className="inline-flex items-center justify-center gap-2 rounded border border-surface-border px-3 py-1.5 text-xs hover:bg-secondary disabled:opacity-50" disabled={!jwt || busy || !canDownload} onClick={() => void download()}>
      {busy ? <Loader2 className="size-3.5 animate-spin" aria-hidden="true" /> : <Download className="size-3.5" aria-hidden="true" />}
      {msg('download')}
    </button>
  </div>;
}
