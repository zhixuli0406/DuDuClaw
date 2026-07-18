import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Empty,
  SettingsSection,
  SettingsCard,
} from '@/components/mds';
import { RowSwitch } from '@/pages/agent-form/form-rows';
import { RefreshCw, Download, CheckCircle, XCircle } from 'lucide-react';

export function UpdateTab() {
  const intl = useIntl();
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState('');
  const [installed, setInstalled] = useState(false);
  const [autoUpdate, setAutoUpdate] = useState(false);
  const [edition, setEdition] = useState('community');
  const [updateInfo, setUpdateInfo] = useState<{
    available: boolean;
    current_version: string;
    latest_version: string;
    release_notes: string;
    published_at: string;
    download_url: string;
    install_method: string;
    brew_formula?: string;
    auto_update?: boolean;
  } | null>(null);

  // Load edition + auto_update state on mount
  useEffect(() => {
    api.system.version().then((info) => {
      setEdition(info.edition ?? 'community');
      setAutoUpdate(info.auto_update ?? false);
    }).catch((e) => {
      console.warn("[api]", e);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    });
  }, [intl]);

  // [H1] useRef guard prevents double-click race — declared before handleCheck
  const installingRef = useRef(false);

  const handleCheck = useCallback(async () => {
    if (installingRef.current) return; // [R2:NM4] block check during install
    setChecking(true);
    setError('');
    setInstalled(false);
    setUpdateInfo(null); // [R2:NL3] clear stale data immediately
    try {
      const info = await api.system.checkUpdate();
      setUpdateInfo(info);
    } catch {
      setError(intl.formatMessage({ id: 'settings.update.failed' }));
    } finally {
      setChecking(false);
    }
  }, [intl]);

  // [M2] applyUpdate no longer sends URL — server uses cached URL from check_update
  const handleInstall = async () => {
    if (installingRef.current || !updateInfo?.download_url) return;
    installingRef.current = true;
    setInstalling(true);
    setError('');
    try {
      const result = await api.system.applyUpdate();
      if (result.success) {
        setInstalled(true);
      } else {
        setError(result.message);
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : '';
      setError(`${intl.formatMessage({ id: 'settings.update.failed' })}${msg ? `: ${msg}` : ''}`);
    } finally {
      setInstalling(false);
      installingRef.current = false;
    }
  };

  const isHomebrew = updateInfo?.install_method === 'homebrew';
  const noBinary = updateInfo?.available && !updateInfo.download_url;
  const isPro = edition !== 'community';

  const handleAutoUpdateToggle = useCallback(async (enabled: boolean) => {
    try {
      await api.system.updateConfig({ auto_update: enabled });
      setAutoUpdate(enabled);
    } catch (e) {
      // state was never flipped, so no revert needed — just surface the failure
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    }
  }, [intl]);

  return (
    <div className="space-y-6">
      {/* Auto-update toggle — Pro only */}
      {isPro && (
        <SettingsSection>
          <SettingsCard>
            <RowSwitch
              label={intl.formatMessage({ id: 'settings.update.autoUpdate' })}
              description={intl.formatMessage({ id: 'settings.update.autoUpdate.desc' })}
              checked={autoUpdate}
              onChange={handleAutoUpdateToggle}
            />
          </SettingsCard>
        </SettingsSection>
      )}

      <div className="flex items-center justify-end">
        <Button variant="brand" size="sm" onClick={handleCheck} disabled={checking}>
          <RefreshCw />
          {checking
            ? intl.formatMessage({ id: 'settings.update.checking' })
            : intl.formatMessage({ id: 'settings.update.check' })}
        </Button>
      </div>

      {/* Status display */}
      {!updateInfo && !error && (
        <Empty
          icon={Download}
          variant="dashed"
          title={intl.formatMessage({ id: 'settings.update.check' })}
        />
      )}

      {error && (
        <div className="rounded-lg bg-destructive/10 p-4 ring-1 ring-inset ring-destructive/20">
          <div className="flex items-center gap-2">
            <XCircle className="h-5 w-5 text-destructive" />
            <span className="text-sm text-destructive">{error}</span>
          </div>
        </div>
      )}

      {installed && (
        <div className="rounded-lg bg-success/10 p-4 ring-1 ring-inset ring-success/20">
          <div className="flex items-center gap-2">
            <CheckCircle className="h-5 w-5 text-success" />
            <span className="text-sm text-success">
              {intl.formatMessage({ id: 'settings.update.installed' })}
            </span>
          </div>
        </div>
      )}

      {updateInfo && !installed && (
        <div className="space-y-4">
          {/* Version info */}
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="rounded-lg bg-muted/50 p-4">
              <span className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'settings.update.current' })}
              </span>
              <p className="mt-1 text-lg font-semibold text-foreground">
                v{updateInfo.current_version}
              </p>
            </div>
            <div className={cn(
              'rounded-lg p-4 ring-1 ring-inset',
              updateInfo.available
                ? 'bg-warning/10 ring-warning/20'
                : 'bg-success/10 ring-success/20'
            )}>
              <span className="text-xs text-muted-foreground">
                {intl.formatMessage({ id: 'settings.update.latest' })}
              </span>
              <p className={cn(
                'mt-1 text-lg font-semibold',
                updateInfo.available
                  ? 'text-warning'
                  : 'text-success'
              )}>
                v{updateInfo.latest_version}
              </p>
            </div>
          </div>

          {!updateInfo.available && (
            <div className="flex items-center gap-2 rounded-lg bg-success/10 p-4 ring-1 ring-inset ring-success/20">
              <CheckCircle className="h-5 w-5 text-success" />
              <span className="text-sm text-success">
                {intl.formatMessage({ id: 'settings.update.upToDate' })}
              </span>
            </div>
          )}

          {updateInfo.available && (
            <>
              {/* Release notes */}
              {updateInfo.release_notes && (
                <div className="rounded-lg p-4 ring-1 ring-inset ring-surface-border">
                  <h4 className="mb-2 text-sm font-medium text-foreground">
                    {intl.formatMessage({ id: 'settings.update.releaseNotes' })}
                  </h4>
                  <pre className="max-h-48 overflow-y-auto whitespace-pre-wrap text-xs text-muted-foreground">
                    {updateInfo.release_notes}
                  </pre>
                </div>
              )}

              {/* Homebrew hint */}
              {isHomebrew && (
                <div className="rounded-lg bg-warning/10 p-4 ring-1 ring-inset ring-warning/20">
                  <p className="text-sm text-warning">
                    {intl.formatMessage({ id: 'settings.update.brewHint' })}
                  </p>
                  <code className="mt-2 block rounded bg-stone-800 px-3 py-2 text-sm text-emerald-400">
                    brew upgrade {updateInfo.brew_formula ?? 'duduclaw'}
                  </code>
                </div>
              )}

              {/* No binary hint */}
              {noBinary && !isHomebrew && (
                <div className="rounded-lg bg-warning/10 p-4 ring-1 ring-inset ring-warning/20">
                  <p className="text-sm text-warning">
                    {intl.formatMessage({ id: 'settings.update.noBinary' })}
                  </p>
                </div>
              )}

              {/* Install button */}
              {!isHomebrew && !noBinary && (
                <Button
                  variant="brand"
                  onClick={handleInstall}
                  disabled={installing}
                  className="w-full py-3"
                >
                  <Download />
                  {installing
                    ? intl.formatMessage({ id: 'settings.update.installing' })
                    : intl.formatMessage({ id: 'settings.update.install' })}
                </Button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
