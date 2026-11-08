import { useIntl } from 'react-intl';
import { AlertTriangle } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';

/** Public guide; the gateway also sends it as `folder_access.docs_url`. */
export const FOLDER_ACCESS_DOCS_URL =
  'https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides/macos-folder-access.md';

/**
 * RM-155: macOS asks again for folder access after an update. Until someone
 * clicks Allow on that Mac (or enables duduclaw in Settings), AI employee
 * reads of protected folders fail or hang. `system.status.folder_access` is
 * present only while that is the case, and is re-broadcast on every change.
 * Admin-only: nobody else can act on the gateway host.
 */
export function FolderAccessBanner() {
  const intl = useIntl();
  const admin = useAuthStore((s) => s.user?.role === 'admin');
  const access = useSystemStore((s) => s.status?.folder_access);
  if (!admin || !access) return null;
  if (access.state !== 'pending_prompt' && access.state !== 'denied') return null;

  const pending = access.state === 'pending_prompt';
  const paths = access.paths.filter((p) => p.state === 'pending_prompt' || p.state === 'denied');
  const docsUrl = access.docs_url || FOLDER_ACCESS_DOCS_URL;

  return (
    <div role="status" className="space-y-1 rounded-lg border border-warning/30 bg-warning/10 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warning" />
        <div className="min-w-0 flex-1 space-y-1">
          <p className="font-medium">
            {intl.formatMessage({ id: pending ? 'folderAccess.pendingTitle' : 'folderAccess.deniedTitle' })}
          </p>
          <p className="text-muted-foreground">{intl.formatMessage({ id: 'folderAccess.steps' })}</p>
          {paths.length > 0 && (
            <ul className="list-inside list-disc text-muted-foreground">
              {paths.map((p) => (
                <li key={p.path_label} className="break-all font-mono text-xs">{p.path_label}</li>
              ))}
            </ul>
          )}
          <a href={docsUrl} target="_blank" rel="noreferrer" className="inline-block text-xs font-medium text-brand underline-offset-2 hover:underline">
            {intl.formatMessage({ id: 'folderAccess.docsLink' })}
          </a>
        </div>
      </div>
    </div>
  );
}
