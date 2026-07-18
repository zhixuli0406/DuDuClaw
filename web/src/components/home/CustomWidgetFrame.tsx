import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { useThemeStore, type Theme } from '@/stores/theme-store';
import { composeWidgetSrcDoc, handleWidgetMessage, type WidgetOutboundMsg } from '@/lib/widget-bridge';
import { Card, CardHeader, CardTitle, CardAction } from '@/components/mds';

/** Effective light/dark for the sandbox document. */
function effectiveMode(theme: Theme): 'light' | 'dark' {
  if (theme === 'dark') return 'dark';
  if (theme === 'light') return 'light';
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches
    ? 'dark'
    : 'light';
}

const MIN_HEIGHT = 80;
const MAX_HEIGHT = 800;

/**
 * Sandboxed renderer for one custom widget (see widget-bridge.ts for the
 * security model). Accepts either a widget id (lazy-loads the HTML — the
 * dashboard path) or inline html (the editor/generator preview path). The frame
 * is wrapped in an MDS Card whose header carries the widget name and an optional
 * `headerAction` (e.g. the Home edit-mode kebab).
 */
export function CustomWidgetFrame({
  widgetId,
  html: inlineHtml,
  title,
  headerAction,
}: {
  widgetId?: string;
  html?: string;
  title?: string;
  /** Optional trailing control in the card header (kebab / actions). */
  headerAction?: ReactNode;
}) {
  const intl = useIntl();
  const theme = useThemeStore((s) => s.theme);
  const mode = effectiveMode(theme);
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const rateWindow = useRef<number[]>([]);
  const [fetched, setFetched] = useState<{ html: string; title: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [height, setHeight] = useState(160);

  useEffect(() => {
    if (!widgetId) return;
    let alive = true;
    api.widgetsCustom
      .get(widgetId)
      .then((w) => alive && setFetched({ html: w.html, title: w.title }))
      .catch((e) => alive && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      alive = false;
    };
  }, [widgetId]);

  const html = inlineHtml ?? fetched?.html ?? null;
  // The srcDoc bakes the INITIAL theme; later changes are pushed live via
  // postMessage so the iframe doesn't remount (and lose state) on toggle.
  const srcDoc = useMemo(() => (html === null ? null : composeWidgetSrcDoc(html, mode)), [html]); // eslint-disable-line react-hooks/exhaustive-deps

  // Bridge: answer RPCs from OUR iframe only; track resize reports.
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const frame = iframeRef.current;
      if (!frame || e.source !== frame.contentWindow) return;
      const msg = e.data as WidgetOutboundMsg;
      if (msg?.type === 'duduclaw:resize' && typeof msg.height === 'number') {
        setHeight(Math.max(MIN_HEIGHT, Math.min(MAX_HEIGHT, Math.ceil(msg.height))));
        return;
      }
      void handleWidgetMessage(msg, rateWindow.current).then((reply) => {
        if (reply && frame.contentWindow) {
          // The sandboxed document has a unique (opaque) origin — '*' is the
          // only deliverable target and leaks nothing beyond this reply.
          frame.contentWindow.postMessage({ type: 'duduclaw:rpc:result', ...reply }, '*');
        }
      });
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  }, []);

  // Push theme changes into the live document.
  useEffect(() => {
    iframeRef.current?.contentWindow?.postMessage({ type: 'duduclaw:theme', mode }, '*');
  }, [mode]);

  const heading = title ?? fetched?.title;

  return (
    <Card className="gap-0 py-0">
      {(heading || headerAction) && (
        <CardHeader className="pt-3 pb-2">
          <CardTitle className="truncate text-sm">{heading}</CardTitle>
          {headerAction && <CardAction>{headerAction}</CardAction>}
        </CardHeader>
      )}
      {error ? (
        <p className="px-4 py-3 text-sm text-muted-foreground">
          {intl.formatMessage({ id: 'widgets.frame.loadError' })}
        </p>
      ) : srcDoc === null ? (
        <div className="h-24 animate-pulse" />
      ) : (
        <iframe
          ref={iframeRef}
          sandbox="allow-scripts"
          srcDoc={srcDoc}
          title={heading ?? 'custom widget'}
          className="block w-full border-0"
          style={{ height }}
        />
      )}
    </Card>
  );
}
