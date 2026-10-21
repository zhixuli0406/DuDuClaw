import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Card, CardContent } from '@/components/mds';
import { useThemeStore, type Theme } from '@/stores/theme-store';
import {
  DISTRIBUTOR_NOTICE_MIN_HEIGHT_PX,
  DISTRIBUTOR_NOTICE_SANDBOX,
  clampNoticeHeight,
  distributorNoticeSrcDoc,
  type NoticeMode,
} from '@/lib/distributor-notice-doc';

function systemDark(): boolean {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-color-scheme: dark)').matches
    : false;
}

/** Light/dark actually in effect, following OS changes in `system` mode. */
function useNoticeMode(): NoticeMode {
  const theme: Theme = useThemeStore((s) => s.theme);
  const [osDark, setOsDark] = useState(systemDark);
  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    const query = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = () => setOsDark(query.matches);
    query.addEventListener('change', onChange);
    return () => query.removeEventListener('change', onChange);
  }, []);
  if (theme === 'dark') return 'dark';
  if (theme === 'light') return 'light';
  return osDark ? 'dark' : 'light';
}

/**
 * Size the frame to its rendered content. The frame is same-origin but runs
 * no script (see distributor-notice-doc.ts), so all measuring happens here:
 * on load, whenever the frame body resizes, and when an image finishes.
 */
function useFrameHeight(frame: React.RefObject<HTMLIFrameElement | null>, srcDoc: string) {
  const [height, setHeight] = useState<number | null>(null);
  const cleanup = useRef<(() => void) | null>(null);

  const measure = useCallback(() => {
    const doc = frame.current?.contentDocument;
    if (!doc?.body) return;
    // The body's own height, not documentElement.scrollHeight: the latter
    // never drops below the current frame height, so the card could only grow.
    setHeight(clampNoticeHeight(Math.max(doc.body.scrollHeight, doc.body.getBoundingClientRect().height)));
  }, [frame]);

  const onLoad = useCallback(() => {
    cleanup.current?.();
    const doc = frame.current?.contentDocument;
    if (!doc?.body) return;
    measure();
    const disposers: Array<() => void> = [];
    if (typeof ResizeObserver === 'function') {
      const observer = new ResizeObserver(() => measure());
      observer.observe(doc.body);
      disposers.push(() => observer.disconnect());
    }
    for (const img of Array.from(doc.images)) {
      img.addEventListener('load', measure);
      disposers.push(() => img.removeEventListener('load', measure));
    }
    window.addEventListener('resize', measure);
    disposers.push(() => window.removeEventListener('resize', measure));
    cleanup.current = () => disposers.forEach((dispose) => dispose());
  }, [frame, measure]);

  // A new document (content or theme change) re-measures on its own load.
  useEffect(() => () => cleanup.current?.(), [srcDoc]);

  return { height, onLoad };
}

/**
 * Distributor information compiled into a distributor build. The HTML is
 * shown only inside a script-less sandboxed iframe (see distributor-notice-doc.ts);
 * the heading and caption are fixed product copy, not part of the notice.
 */
export function DistributorNoticeCard({ html }: { html: string }) {
  const intl = useIntl();
  const mode = useNoticeMode();
  const frame = useRef<HTMLIFrameElement>(null);
  const srcDoc = useMemo(() => distributorNoticeSrcDoc(html, mode), [html, mode]);
  const { height, onLoad } = useFrameHeight(frame, srcDoc);
  const title = intl.formatMessage({ id: 'about.distributor.title' });

  return (
    <Card>
      <CardContent>
        <p className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{title}</p>
        <p className="mt-1 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'about.distributor.caption' })}
        </p>
        <iframe
          ref={frame}
          title={title}
          sandbox={DISTRIBUTOR_NOTICE_SANDBOX}
          srcDoc={srcDoc}
          onLoad={onLoad}
          referrerPolicy="no-referrer"
          data-measured={height === null ? 'false' : 'true'}
          className="mt-3 block w-full rounded-md border-0 motion-safe:transition-[height] motion-safe:duration-150 motion-safe:ease-out"
          style={{ height: height ?? DISTRIBUTOR_NOTICE_MIN_HEIGHT_PX }}
        />
      </CardContent>
    </Card>
  );
}
