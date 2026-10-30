import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, Download, Loader2 } from 'lucide-react';
import { api, type RuntimeDetect } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useRuntimeInstall } from '@/hooks/useRuntimeInstall';
import { Button } from '@/components/mds';

export function CodexUpdateBanner() {
  const admin = useAuthStore((s) => s.user?.role === 'admin');
  return admin ? <AdminCodexUpdateBanner /> : null;
}

function AdminCodexUpdateBanner() {
  const intl = useIntl();
  const [row, setRow] = useState<NonNullable<RuntimeDetect['runtimes']>[number]>();
  const [copied, setCopied] = useState(false);
  const mounted = useRef(false);
  const refresh = useCallback(() => {
    void api.runtime.detect().then((result) => {
      if (mounted.current) setRow(result.runtimes?.find((r) => r.id === 'codex' && r.update_required));
    }).catch(() => { /* Keep the last known requirement while offline. */ });
  }, []);
  useEffect(() => {
    mounted.current = true;
    refresh();
    return () => { mounted.current = false; };
  }, [refresh]);
  const { phase, output, manual, startInstall } = useRuntimeInstall('codex', refresh);
  const busy = phase === 'running';
  if (!row) return null;

  return (
    <div role="status" className="mb-4 space-y-3 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
        <div className="min-w-0 flex-1 space-y-1">
          <p className="font-medium">{intl.formatMessage({ id: 'runtimeUpdate.title' })}</p>
          <p className="text-muted-foreground">
            {intl.formatMessage({ id: 'runtimeUpdate.description' }, {
              version: row.version ?? intl.formatMessage({ id: 'runtimeUpdate.unknownVersion' }),
            })}
          </p>
        </div>
        <Button variant="outline" disabled={busy} onClick={() => { setCopied(false); void startInstall(); }}>
          {busy ? <Loader2 className="animate-spin motion-reduce:animate-none" /> : <Download />}
          {intl.formatMessage({ id: busy ? 'welcome.runtime.installing' : 'runtimeUpdate.action' })}
        </Button>
      </div>
      {(busy || output) && (
        <pre className="max-h-32 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted p-3 font-mono text-xs">
          {output || intl.formatMessage({ id: 'welcome.runtime.installStarting' })}
        </pre>
      )}
      {manual && (
        <div className="space-y-2">
          <p>{manual.message}</p>
          <p className="text-muted-foreground">{intl.formatMessage({ id: 'welcome.runtime.manualHint' })}</p>
          <code className="block select-all break-all rounded-lg bg-muted p-2">{manual.command}</code>
          <Button variant="outline" size="sm" onClick={() => {
            void navigator.clipboard?.writeText(manual.command).then(() => setCopied(true)).catch(() => setCopied(false));
          }}>
            {intl.formatMessage({ id: copied ? 'welcome.runtime.copied' : 'welcome.runtime.copy' })}
          </Button>
          <Button variant="ghost" size="sm" onClick={refresh}>
            {intl.formatMessage({ id: 'welcome.runtime.redetect' })}
          </Button>
        </div>
      )}
    </div>
  );
}
