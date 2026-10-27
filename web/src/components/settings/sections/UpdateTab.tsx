import { useEffect, useState, useCallback, useRef } from 'react';
import { useIntl } from 'react-intl';
import { cn } from '@/lib/utils';
import { api } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { toast, formatError } from '@/lib/toast';
import {
  Button,
  Empty,
  SettingsSection,
  SettingsCard,
} from '@/components/mds';
import { RowSwitch } from '@/pages/agent-form/form-rows';
import {
  desktopUpdateInfo,
  restartAndUpdateDesktop,
  type DesktopUpdateInfo,
} from '@/lib/desktop-update';
import {
  compareVersions,
  planUpdate,
  rememberDesktopResume,
  takeDesktopResume,
} from '@/lib/update-plan';
import { RefreshCw, Download, CheckCircle, XCircle } from 'lucide-react';

/**
 * Should the failure box add 「通常是網路或發布傳播延遲，稍後再試」?
 *
 * Only for download-stage failures — the ones the gateway already retried twice
 * (`updater::FailureClass::Transient`) before surfacing them, where waiting
 * genuinely is the fix. Deliberately an ALLOWLIST of the download-stage message
 * shapes (`updater::fetch_asset_bytes`) rather than a denylist of integrity
 * errors: an unrecognised failure gets no advice instead of possibly-wrong
 * advice, and telling someone to "try again later" after a checksum or
 * signature mismatch would paper over exactly the case that must fail closed.
 */
export function showsNetworkHint(message: string): boolean {
  // NOT anchored: the caller prefixes the gateway text with a localised
  // 「更新失敗: 」, so an anchored pattern would never match in practice.
  if (/download failed:/i.test(message)) return true;
  if (/Failed to read (Update archive|Checksum file|Signature file)/i.test(message)) return true;
  const http = message.match(/download returned HTTP (\d{3})/i);
  // Mirrors `updater::classify_http_status` — a 410 Gone is a permanent answer
  // and must not be dressed up as "the release hasn't propagated yet".
  if (http) return RETRYABLE_HTTP.has(Number(http[1])) || Number(http[1]) >= 500;
  return false;
}

/** Non-5xx statuses the gateway treats as retryable (`classify_http_status`). */
const RETRYABLE_HTTP = new Set([403, 404, 408, 425, 429]);

/**
 * Which "already on disk, restart to apply" banner fits this gateway. Only a
 * standalone gateway gets the "restarting the desktop app does not restart
 * the gateway" wording: a desktop sidecar does restart with the app, and a
 * container restarts by recreating it from the new image.
 */
export function restartPendingMessageId(info: {
  install_method: string;
  containerized?: boolean;
}): string {
  if (info.containerized === true) return 'settings.update.restartPendingContainer';
  if (info.install_method === 'desktop') return 'settings.update.restartPendingDesktop';
  return 'settings.update.restartPending';
}

/** Live download-retry state pushed by the gateway during an install. */
interface RetryState {
  attempt: number;
  maxAttempts: number;
}

export function UpdateTab() {
  const intl = useIntl();
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState('');
  /** Non-null only while the gateway is on a RETRY (attempt ≥ 2). */
  const [retry, setRetry] = useState<RetryState | null>(null);
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
    restart_pending_version?: string | null;
    update_channel?: 'control_plane' | 'github' | 'none';
    containerized?: boolean;
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

  // Desktop shell only: the shell's own version, the shell update already
  // downloaded and waiting for a restart, and whether the shell owns its
  // gateway. The update-ready notification sends users here, so the
  // 「重啟並更新」affordance lives on this page (2026-08-05 report). Attached
  // to an external gateway, the app update is ordered after the gateway's
  // (2026-10-27 report). Null outside the shell, so browsers see no change.
  const [desktop, setDesktop] = useState<DesktopUpdateInfo | null>(null);
  const stagedVersion = desktop?.stagedVersion ?? null;
  const desktopExternal = desktop?.gateway === 'external';
  const [restarting, setRestarting] = useState(false);
  useEffect(() => {
    let cancelled = false;
    const refresh = () =>
      desktopUpdateInfo().then((info) => {
        if (!cancelled) setDesktop(info);
      });
    void refresh();
    // The background download can finish while the tab is open.
    const timer = setInterval(refresh, 30_000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  const handleRestartAndUpdate = useCallback(async () => {
    setRestarting(true);
    try {
      // Success never resolves — the shell relaunches out from under us.
      await restartAndUpdateDesktop();
    } catch (err) {
      setRestarting(false);
      toast.error(
        intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(err) }),
      );
    }
  }, [intl]);

  // Second half of a combined update: the page reloaded once the gateway came
  // back. Update the app only if the gateway really reached its version.
  useEffect(() => {
    const resume = takeDesktopResume();
    if (!resume) return;
    let cancelled = false;
    void (async () => {
      try {
        const [{ version }, info] = await Promise.all([api.system.version(), desktopUpdateInfo()]);
        if (cancelled) return;
        const staged = info?.stagedVersion;
        if (staged && compareVersions(staged, version) <= 0) {
          await handleRestartAndUpdate();
        } else if (staged) {
          toast.info(intl.formatMessage({ id: 'settings.update.desktopHeld' }, { version: staged }));
        }
      } catch (err) {
        toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(err) }));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [handleRestartAndUpdate, intl]);

  // The gateway retries a failed download twice (5s / 15s back-off). Without
  // this the button would sit on a silent "安裝中…" for up to 20 extra seconds
  // and then flash red, which is exactly what made the 2026-08-04 field report
  // read as "it just failed" — the user had no way to know a retry was running.
  useEffect(() =>
    client.subscribe('system.update_progress', (payload) => {
      const p = payload as { attempt?: number; max_attempts?: number };
      if (typeof p?.attempt !== 'number' || typeof p?.max_attempts !== 'number') return;
      // Attempt 1 is the ordinary first try — nothing to announce.
      setRetry(p.attempt > 1 ? { attempt: p.attempt, maxAttempts: p.max_attempts } : null);
    }),
  []);

  const handleCheck = useCallback(async () => {
    if (installingRef.current) return; // [R2:NM4] block check during install
    setChecking(true);
    setError('');
    setRetry(null);
    setInstalled(false);
    setUpdateInfo(null); // [R2:NL3] clear stale data immediately
    try {
      const info = await api.system.checkUpdate();
      setUpdateInfo(info);
    } catch (err) {
      // Surface the server's reason — a bare 「更新失敗」 hid the actual
      // cause (e.g. GitHub API rate limit) from the 2026-07-29 field report.
      const msg = err instanceof Error ? err.message : String(err ?? '');
      setError(`${intl.formatMessage({ id: 'settings.update.failed' })}${msg ? `: ${msg}` : ''}`);
    } finally {
      setChecking(false);
    }
  }, [intl]);

  // [M2] applyUpdate no longer sends URL — server uses cached URL from check_update
  // One button, one order: the gateway step first (install, or restart onto
  // a binary already on disk), then the desktop app up to the gateway's
  // version. The gateway step reloads the page, so the app step resumes
  // after the reload (see the resume effect above).
  const handleInstall = async () => {
    if (installingRef.current || !plan) return;
    installingRef.current = true;
    setInstalling(true);
    setError('');
    setRetry(null);
    try {
      if (plan.gatewayStep === 'none') {
        if (plan.desktopStep === 'install') await handleRestartAndUpdate();
        return;
      }
      if (plan.desktopStep === 'install' && stagedVersion) rememberDesktopResume(stagedVersion);
      const result =
        plan.gatewayStep === 'restart'
          ? await api.system.restartPendingUpdate()
          : await api.system.applyUpdate();
      if (result.success) {
        setInstalled(true);
        // Only a restart reloads this page and resumes the app step.
        if (!result.needs_restart) takeDesktopResume();
      } else {
        takeDesktopResume();
        setError('message' in result ? result.message : '');
      }
    } catch (err) {
      takeDesktopResume();
      const msg = err instanceof Error ? err.message : '';
      setError(`${intl.formatMessage({ id: 'settings.update.failed' })}${msg ? `: ${msg}` : ''}`);
    } finally {
      setInstalling(false);
      setRetry(null);
      installingRef.current = false;
    }
  };

  const isHomebrew = updateInfo?.install_method === 'homebrew';
  const isDesktop = updateInfo?.install_method === 'desktop';
  // Containerized deployment: the image is immutable, so an in-process
  // binary swap never sticks regardless of `update_channel` — this check
  // takes priority over `isProNoChannel` below (a containerized host may
  // legitimately report update_channel "none", but the guidance is
  // different: run update.sh / rebuild the image, not "wait for a channel").
  const isContainerized = updateInfo?.containerized === true;
  // Enterprise binary with no update channel configured: the new version is
  // real (the enterprise build follows the same release train), but this
  // process cannot install it — the binary arrives through the enterprise
  // distribution package or a rebuild. Offering an install button here would
  // only produce a guaranteed refusal.
  const isProNoChannel =
    updateInfo?.install_method === 'pro' && updateInfo?.update_channel === 'none' && !isContainerized;
  // A control-plane provider resolves its own asset at install time, so an
  // empty download_url there says nothing about availability. `noBinary` stays
  // a statement about the public release channel only (undefined channel = an
  // older gateway = the public channel).
  const usesProvider = updateInfo?.update_channel === 'control_plane';
  const noBinary =
    updateInfo?.available &&
    !updateInfo.download_url &&
    !isProNoChannel &&
    !usesProvider &&
    !isContainerized;
  const isPro = edition !== 'community';
  // A control-plane provider resolves its asset server-side at install time,
  // so it legitimately reports no download_url — the gateway holds the cached
  // descriptor either way (`system.apply_update` never takes a URL from us).
  const gatewayInstallable =
    !isHomebrew &&
    !isDesktop &&
    !noBinary &&
    !isProNoChannel &&
    !isContainerized &&
    (!!updateInfo?.download_url || usesProvider);
  const plan = updateInfo
    ? planUpdate(
        {
          currentVersion: updateInfo.current_version,
          latestVersion: updateInfo.latest_version,
          available: updateInfo.available,
          installable: gatewayInstallable,
          // Restarting is the gateway's own job; containers and the desktop
          // sidecar restart through their owner instead.
          restartPendingVersion:
            isContainerized || isDesktop ? null : (updateInfo.restart_pending_version ?? null),
        },
        desktop,
      )
    : null;
  const installLabel = (() => {
    if (!plan) return '';
    if (plan.gatewayStep === 'restart') {
      return intl.formatMessage(
        { id: 'settings.update.restartGateway' },
        { version: plan.gatewayVersionAfter },
      );
    }
    if (plan.gatewayStep === 'install' && plan.desktopStep === 'install') {
      return intl.formatMessage({ id: 'settings.update.installBoth' });
    }
    if (plan.gatewayStep === 'none') {
      return intl.formatMessage({ id: 'settings.update.restartAndUpdate' });
    }
    return intl.formatMessage({ id: 'settings.update.install' });
  })();
  const showInstall =
    !!plan && (plan.gatewayStep !== 'none' || (desktopExternal && plan.desktopStep === 'install'));

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

      {/* Desktop shell update staged and ready — the in-page counterpart of
          the tray「重啟並更新」item. Shown unconditionally (before any manual
          check): the user typically lands here straight from the update-ready
          notification. */}
      {stagedVersion && desktopExternal && (
        <div className="flex items-center gap-3 rounded-lg bg-warning/10 p-4 ring-1 ring-inset ring-warning/20">
          <RefreshCw className="h-5 w-5 shrink-0 text-warning" />
          <span className="min-w-0 flex-1 text-sm text-warning">
            {intl.formatMessage(
              { id: 'settings.update.desktopStagedExternal' },
              { version: stagedVersion },
            )}
          </span>
        </div>
      )}
      {stagedVersion && !desktopExternal && (
        <div className="flex flex-wrap items-center gap-3 rounded-lg bg-warning/10 p-4 ring-1 ring-inset ring-warning/20">
          <RefreshCw className="h-5 w-5 shrink-0 text-warning" />
          <span className="min-w-0 flex-1 text-sm text-warning">
            {intl.formatMessage(
              { id: 'settings.update.desktopStaged' },
              { version: stagedVersion },
            )}
          </span>
          <Button variant="brand" size="sm" onClick={handleRestartAndUpdate} disabled={restarting}>
            {restarting
              ? intl.formatMessage({ id: 'settings.update.restarting' })
              : intl.formatMessage({ id: 'settings.update.restartAndUpdate' })}
          </Button>
        </div>
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
            <XCircle className="h-5 w-5 shrink-0 text-destructive" />
            <span className="text-sm text-destructive">{error}</span>
          </div>
          {/* The gateway already retried twice before this landed. Say what the
              remaining likely cause is — but never on an integrity failure,
              where "try again later" would be the wrong thing to do. */}
          {showsNetworkHint(error) && (
            <p className="mt-2 pl-7 text-xs text-destructive/80">
              {intl.formatMessage({ id: 'settings.update.failedHint' })}
            </p>
          )}
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
          {/* Binary already swapped on disk (npm/brew/manual) — the running
              gateway is older than what's installed; a restart is all that's
              missing. Shown regardless of `available`. */}
          {updateInfo.restart_pending_version && (
            <div className="flex items-center gap-2 rounded-lg bg-warning/10 p-4 ring-1 ring-inset ring-warning/20">
              <RefreshCw className="h-5 w-5 shrink-0 text-warning" />
              <span className="text-sm text-warning">
                {intl.formatMessage(
                  { id: restartPendingMessageId(updateInfo) },
                  { version: updateInfo.restart_pending_version },
                )}
              </span>
            </div>
          )}
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

          {/* Desktop shell attached to an external gateway: show both
              installs so the app's version never looks like the gateway's. */}
          {desktopExternal && plan && (
            <div className="space-y-2 rounded-lg p-4 ring-1 ring-inset ring-surface-border">
              <h4 className="text-sm font-medium text-foreground">
                {intl.formatMessage({ id: 'settings.update.components' })}
              </h4>
              <div className="flex flex-wrap items-baseline justify-between gap-2 text-sm">
                <span className="text-muted-foreground">
                  {intl.formatMessage({ id: 'settings.update.component.gateway' })}
                </span>
                <span className="text-foreground">
                  v{updateInfo.current_version}
                  {plan.gatewayStep !== 'none' && ` → v${plan.gatewayVersionAfter}`}
                </span>
              </div>
              <div className="flex flex-wrap items-baseline justify-between gap-2 text-sm">
                <span className="text-muted-foreground">
                  {intl.formatMessage({ id: 'settings.update.component.desktop' })}
                </span>
                <span className="text-foreground">
                  {desktop?.currentVersion ? `v${desktop.currentVersion}` : '—'}
                  {plan.desktopStep === 'install' && stagedVersion && ` → v${stagedVersion}`}
                </span>
              </div>
              {plan.desktopStep === 'hold' && stagedVersion && (
                <p className="text-xs text-muted-foreground">
                  {intl.formatMessage(
                    { id: 'settings.update.desktopHeld' },
                    { version: stagedVersion },
                  )}
                </p>
              )}
            </div>
          )}

          {!updateInfo.available && !showInstall && (
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

              {/* Homebrew installs: tap is frozen, guide the user to migrate
                  to npm or the desktop app instead of a stale `brew upgrade`. */}
              {isHomebrew && (
                <div className="rounded-lg bg-warning/10 p-4 ring-1 ring-inset ring-warning/20">
                  <p className="text-sm text-warning">
                    {intl.formatMessage({ id: 'settings.update.brewHint' })}
                  </p>
                </div>
              )}

              {/* Desktop app: updates arrive via the app's own updater */}
              {isDesktop && (
                <div className="rounded-lg bg-muted/60 p-4 ring-1 ring-inset ring-surface-border">
                  <p className="text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'settings.update.desktopManaged' })}
                  </p>
                </div>
              )}

              {/* Containerized deployment — image is immutable, no in-process
                  install is possible regardless of update_channel. Takes
                  priority over every other guidance card below. */}
              {isContainerized && (
                <div className="rounded-lg bg-muted/60 p-4 ring-1 ring-inset ring-surface-border">
                  <p className="text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'settings.update.containerHint' })}
                  </p>
                </div>
              )}

              {/* Enterprise binary, no update channel configured */}
              {isProNoChannel && !isContainerized && (
                <div className="rounded-lg bg-muted/60 p-4 ring-1 ring-inset ring-surface-border">
                  <p className="text-sm text-muted-foreground">
                    {intl.formatMessage({ id: 'settings.update.proNoChannel' })}
                  </p>
                </div>
              )}

              {/* No binary hint */}
              {noBinary && !isHomebrew && !isDesktop && !isContainerized && (
                <div className="rounded-lg bg-warning/10 p-4 ring-1 ring-inset ring-warning/20">
                  <p className="text-sm text-warning">
                    {intl.formatMessage({ id: 'settings.update.noBinary' })}
                  </p>
                </div>
              )}

            </>
          )}

          {/* One install button for the whole order (gateway, then app). */}
          {showInstall && (
            <Button
              variant="brand"
              onClick={handleInstall}
              disabled={installing || restarting}
              className="w-full py-3"
            >
              <Download />
              {!installing
                ? installLabel
                : retry
                  ? intl.formatMessage(
                      { id: 'settings.update.retrying' },
                      { attempt: retry.attempt, total: retry.maxAttempts },
                    )
                  : intl.formatMessage({ id: 'settings.update.installing' })}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
