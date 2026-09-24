import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { AlertTriangle, Download, RefreshCw } from 'lucide-react';
import { api, type RedactionModelStatus, type RedactionProfileInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Badge, Button } from '@/components/mds';
import { categoryLabel, ToneBadge } from './RedactionTab';
import {
  AI_DETECT_CATEGORIES,
  deriveAiDetectView,
  formatModelBytes,
  formatProgressCaption,
  needsNotReadyWarning,
  shouldPollModelStatus,
  type AiDetectView,
} from './redactionAiDetection';

const POLL_MS = 2000;

// ── "AI 智慧偵測" row (WP-N2, canvas screen 1) ───────────────────────────
//
// DESIGN-redaction-ner-and-custom-rules-2026-09 §12.1/§13.4. Renders IN
// PLACE of RedactionTab's generic checkbox+categories `<label>` row for the
// one profile whose `requires_model` is true (the built-in `ai_pii`
// profile) — every other profile is unaffected. Owns its own
// `redaction.model.status` fetch/poll: model install/download state changes
// on its own clock (a background download), independent of `redaction.get`
// and of the parent form's batched Save button.
export function RedactionAiDetectionRow({
  profile,
  checked,
  onToggle,
  categoryLabels,
}: {
  profile: RedactionProfileInfo;
  checked: boolean;
  onToggle: () => void;
  /** `RedactionConfig.category_labels`, passed straight through to
   *  `categoryLabel` for the expanded row's 8 category chips. */
  categoryLabels?: Record<string, string>;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);

  const [status, setStatus] = useState<RedactionModelStatus | null>(null);
  // Distinct from the backend's own `state: "error"` — this means the
  // `redaction.model.status` CALL itself failed (network/auth/RPC), not that
  // the model download failed. See `AiDetectView`'s `unknown` doc comment.
  const [loadFailed, setLoadFailed] = useState(false);
  const [busy, setBusy] = useState<'install' | 'cancel' | null>(null);
  const pollTimer = useRef<ReturnType<typeof setInterval> | null>(null);

  const view: AiDetectView = loadFailed ? { kind: 'unknown' } : deriveAiDetectView(status);

  const refreshStatus = useCallback(() => {
    return api.redaction.model
      .status()
      .then((s) => {
        setStatus(s);
        setLoadFailed(false);
      })
      .catch(() => setLoadFailed(true));
  }, []);

  // Initial fetch, and a from-scratch poll cleanup on unmount (the reactive
  // start/stop effect below handles every mid-life transition).
  useEffect(() => {
    void refreshStatus();
    return () => {
      if (pollTimer.current) {
        clearInterval(pollTimer.current);
        pollTimer.current = null;
      }
    };
  }, [refreshStatus]);

  // Single source of truth for "is the 2s poll running right now" — reacts
  // to every state transition (a fresh `handleInstall` click, `handleCancel`
  // finishing, or the download itself completing/failing on its own),
  // so there's no separate "stop polling" call scattered elsewhere.
  useEffect(() => {
    if (shouldPollModelStatus(view)) {
      if (!pollTimer.current) pollTimer.current = setInterval(() => void refreshStatus(), POLL_MS);
    } else if (pollTimer.current) {
      clearInterval(pollTimer.current);
      pollTimer.current = null;
    }
  }, [view, refreshStatus]);

  const handleInstall = () => {
    setBusy('install');
    api.redaction.model
      .install()
      .then(() => {
        toast.success(t('redaction.aiDetect.install.started'));
        return refreshStatus();
      })
      .catch((e) => toast.error(t('redaction.aiDetect.install.error', { message: formatError(e) })))
      .finally(() => setBusy(null));
  };

  const handleCancel = () => {
    setBusy('cancel');
    api.redaction.model
      .cancel()
      .then(() => refreshStatus())
      .catch((e) => toast.error(t('redaction.aiDetect.cancel.error', { message: formatError(e) })))
      .finally(() => setBusy(null));
  };

  // Same name-resolution rule RedactionTab's own profile loop uses for every
  // BUILT-IN profile: the curated `redaction.profile.<name>` i18n string,
  // never the raw untranslated `[meta] name` in `profile.label`.
  const name = intl.formatMessage({ id: 'redaction.profile.ai_pii', defaultMessage: profile.label || profile.name });

  return (
    <div className="rounded-lg bg-muted/50 p-2.5">
      <div className="flex items-start gap-2.5">
        <label className="flex min-w-0 flex-1 cursor-pointer items-start gap-2.5">
          <input
            type="checkbox"
            checked={checked}
            onChange={onToggle}
            className="mt-0.5 h-4 w-4 accent-primary"
          />
          <span className="min-w-0 flex-1">
            <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium text-foreground">
              {name}
              <Badge className="bg-brand/10 text-brand">{t('redaction.aiDetect.chip.localModel')}</Badge>
            </span>
            <span className="mt-1 block text-xs text-muted-foreground">{t('redaction.aiDetect.desc')}</span>
          </span>
        </label>

        {/* Status area — deliberately OUTSIDE the <label>, so clicking the
            download/cancel/retry button never also toggles the checkbox. */}
        <div className="flex shrink-0 flex-col items-end gap-1.5">
          {view.kind === 'unknown' && (
            <ToneBadge tone="neutral">{t('redaction.aiDetect.status.unknown')}</ToneBadge>
          )}

          {view.kind === 'absent' && (
            <>
              <ToneBadge tone="warning" dot>
                {t('redaction.aiDetect.status.absent', { size: formatModelBytes(view.sizeBytes) })}
              </ToneBadge>
              <Button variant="brand" size="sm" disabled={busy === 'install'} onClick={handleInstall}>
                <Download />
                {t('redaction.aiDetect.status.absent.button')}
              </Button>
            </>
          )}

          {view.kind === 'downloading' && (
            <>
              <span className="whitespace-nowrap text-xs text-muted-foreground">
                {t('redaction.aiDetect.status.downloading', {
                  progress: formatProgressCaption(view.doneBytes, view.totalBytes),
                })}
              </span>
              <div
                className="h-1.5 w-36 overflow-hidden rounded-full bg-muted"
                role="progressbar"
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={view.pct ?? undefined}
              >
                <div
                  className="h-full rounded-full bg-brand transition-all"
                  style={{ width: view.pct != null ? `${view.pct}%` : '10%' }}
                />
              </div>
              <Button variant="ghost" size="xs" disabled={busy === 'cancel'} onClick={handleCancel}>
                {t('redaction.aiDetect.status.cancel')}
              </Button>
            </>
          )}

          {view.kind === 'ready' && (
            <>
              <ToneBadge tone="success" dot>
                {view.revision
                  ? t('redaction.aiDetect.status.ready', { revision: view.revision })
                  : t('redaction.aiDetect.status.ready.noRevision')}
              </ToneBadge>
              <span className="text-xs text-muted-foreground">
                {view.hasUsage && view.avgLatencyMs != null
                  ? t('redaction.aiDetect.status.avgLatency', { ms: Math.round(view.avgLatencyMs) })
                  : t('redaction.aiDetect.status.notUsedYet')}
              </span>
            </>
          )}

          {view.kind === 'error' && (
            <>
              <ToneBadge tone="danger">{t('redaction.aiDetect.status.error')}</ToneBadge>
              <Button variant="outline" size="sm" disabled={busy === 'install'} onClick={handleInstall}>
                <RefreshCw />
                {t('redaction.aiDetect.status.retry')}
              </Button>
            </>
          )}
        </div>
      </div>

      {needsNotReadyWarning(view, checked) && (
        <p className="mt-2 flex items-start gap-1.5 text-xs text-warning">
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          {t('redaction.aiDetect.warnNotReady')}
        </p>
      )}

      {/* Expanded content (§12.1): shown whenever the row is checked,
          regardless of install state — it's what the operator is opting
          into, not a status of the model itself. */}
      {checked && (
        <div className="mt-3 space-y-2 border-t border-dashed border-surface-border pt-2.5">
          <div className="flex flex-wrap gap-1.5">
            {AI_DETECT_CATEGORIES.map((cat) => (
              <Badge key={cat} className="bg-brand/10 text-brand">
                {categoryLabel(intl, cat, categoryLabels)}
              </Badge>
            ))}
          </div>
          <div className="flex gap-2 rounded-lg border border-warning/40 bg-warning/10 px-3 py-2 text-xs">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-warning" />
            <div className="space-y-0.5">
              <p className="font-medium text-foreground">{t('redaction.aiDetect.alert.title')}</p>
              <p className="text-muted-foreground">{t('redaction.aiDetect.alert.body')}</p>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
