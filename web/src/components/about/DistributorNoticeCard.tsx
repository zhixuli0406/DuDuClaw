import { useEffect, useMemo, useState } from 'react';
import { useIntl } from 'react-intl';
import { Card, CardContent } from '@/components/mds';
import { useThemeStore, type Theme } from '@/stores/theme-store';
import {
  DISTRIBUTOR_NOTICE_HEIGHT_PX,
  DISTRIBUTOR_NOTICE_SANDBOX,
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
 * Distributor information compiled into a distributor build. The HTML is
 * shown only inside a script-less sandboxed iframe (see distributor-notice-doc.ts);
 * the heading and caption are fixed product copy, not part of the notice.
 */
export function DistributorNoticeCard({ html }: { html: string }) {
  const intl = useIntl();
  const mode = useNoticeMode();
  const srcDoc = useMemo(() => distributorNoticeSrcDoc(html, mode), [html, mode]);
  const title = intl.formatMessage({ id: 'about.distributor.title' });

  return (
    <Card>
      <CardContent>
        <p className="text-[11px] font-medium uppercase tracking-wider text-muted-foreground">{title}</p>
        <p className="mt-1 text-xs text-muted-foreground">
          {intl.formatMessage({ id: 'about.distributor.caption' })}
        </p>
        <iframe
          title={title}
          sandbox={DISTRIBUTOR_NOTICE_SANDBOX}
          srcDoc={srcDoc}
          referrerPolicy="no-referrer"
          className="mt-3 block w-full rounded-md border-0"
          style={{ height: DISTRIBUTOR_NOTICE_HEIGHT_PX }}
        />
      </CardContent>
    </Card>
  );
}
