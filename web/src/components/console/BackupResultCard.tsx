import { useIntl } from 'react-intl';
import { Archive, Download } from 'lucide-react';
import { useAuthStore } from '@/stores/auth-store';
import { Button } from '@/components/mds';
import { timeAgo } from '@/lib/format';
import type { BackupResultArtifact } from './artifact-types';
import { ArtifactShell } from './ArtifactShell';
import { formatBytes } from './format';

/** Triggers a same-origin file download via a throwaway `<a download>` —
 *  identical trick to `DevicePage.tsx`'s `runBackup` / `downloadBackup`
 *  (browsers refuse a `fetch`+`Authorization`-header download to save
 *  itself, so the JWT rides along as a query param instead). */
function triggerDownload(href: string, filename: string) {
  const a = document.createElement('a');
  a.href = href;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/**
 * 備份卡 — the conversational twin of DevicePage's ④備份卡. Two modes:
 *  - `created`: the result of a just-run `device.backup_create` — one
 *    "下載" button for the fresh archive.
 *  - `list`: the result of `device.backup_list` — the existing archives,
 *    each with its own download button.
 */
export function BackupResultCard({ payload }: { payload: BackupResultArtifact['payload'] }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const jwt = useAuthStore((s) => s.jwt);

  const download = (name: string, base: '/api/files/download' | '/api/device/backups/download') => {
    const params = new URLSearchParams({ name });
    if (jwt) params.set('token', jwt);
    triggerDownload(`${base}?${params.toString()}`, name);
  };

  if (payload.mode === 'created') {
    const { filename } = payload.result;
    return (
      <ArtifactShell icon={Archive} title={t('console.artifact.backupResult.title.created')} advancedTo="/device">
        <div className="flex items-center justify-between gap-3 rounded-lg border border-surface-border px-3 py-2">
          <span className="min-w-0 truncate font-mono text-xs text-foreground">{filename}</span>
          <Button variant="brand" size="sm" onClick={() => download(filename, '/api/files/download')}>
            <Download />
            {t('files.download')}
          </Button>
        </div>
      </ArtifactShell>
    );
  }

  return (
    <ArtifactShell icon={Archive} title={t('console.artifact.backupResult.title.list')} advancedTo="/device">
      {payload.files.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t('device.backup.list.empty')}</p>
      ) : (
        <div className="space-y-1.5">
          {payload.files.map((f) => (
            <div
              key={f.name}
              className="flex items-center justify-between gap-2 rounded-lg border border-surface-border px-2.5 py-1.5"
            >
              <div className="min-w-0">
                <p className="truncate font-mono text-xs text-foreground">{f.name}</p>
                <p className="text-xs text-muted-foreground">
                  {formatBytes(f.size)} · {timeAgo(f.mtime)}
                </p>
              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                onClick={() => download(f.name, '/api/device/backups/download')}
                aria-label={t('files.download')}
                title={t('files.download')}
              >
                <Download />
              </Button>
            </div>
          ))}
        </div>
      )}
    </ArtifactShell>
  );
}
