import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { Download, RefreshCw, ShieldCheck, Trash2 } from 'lucide-react';
import { Badge, Button, Input, Segmented } from '@/components/mds';
import { toast, formatError } from '@/lib/toast';
import {
  knowledgeEmbeddingApi,
  type KbEmbeddingModel,
  type KbEmbeddingProvider,
  type KbEmbeddingSettings,
} from '@/lib/knowledge-embedding-api';
import { EmbeddingDisclosureDialog } from './EmbeddingDisclosureDialog';
import { EMBEDDING_POLL_MS, disclosureTarget, formatBytes, isCloud, percent, shouldPoll } from './embedding-settings';

/**
 * EmbeddingSettingsCard — how the document knowledge base finds passages by
 * meaning. Shows whether that works right now (and, when it does not, the
 * reason in plain words), the local models with install / remove / offline
 * check, the provider choice, and coverage. A cloud provider can only be
 * chosen through the disclosure dialog.
 */
export function EmbeddingSettingsCard() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const [s, setS] = useState<KbEmbeddingSettings | null>(null);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [provider, setProvider] = useState<KbEmbeddingProvider>('local');
  const [cloudModel, setCloudModel] = useState('');
  const [cloudAccount, setCloudAccount] = useState('');
  const [disclose, setDisclose] = useState(false);
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);

  const refresh = useCallback(
    () =>
      knowledgeEmbeddingApi
        .status()
        .then((v) => {
          setS(v);
          setFailed(false);
        })
        .catch(() => setFailed(true)),
    [],
  );

  useEffect(() => {
    void refresh().then(() => undefined);
    return () => {
      if (timer.current) clearInterval(timer.current);
    };
  }, [refresh]);

  // Keep the form in step with what is saved.
  useEffect(() => {
    if (s) setProvider(s.provider);
  }, [s?.provider]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (shouldPoll(s)) {
      if (!timer.current) timer.current = setInterval(() => void refresh(), EMBEDDING_POLL_MS);
    } else if (timer.current) {
      clearInterval(timer.current);
      timer.current = null;
    }
  }, [s, refresh]);

  const run = (key: string, p: Promise<unknown>, ok?: string) => {
    setBusy(key);
    return p
      .then(() => {
        if (ok) toast.success(ok);
        return refresh();
      })
      .catch((e) => toast.error(t('kb.embedding.error', { message: formatError(e) })))
      .finally(() => setBusy(null));
  };

  if (failed && !s) {
    return <p className="text-sm text-muted-foreground">{t('kb.embedding.loadFailed')}</p>;
  }
  if (!s) return <p className="text-sm text-muted-foreground">{t('kb.embedding.loading')}</p>;

  const admin = s.is_admin;
  const save = (ack: boolean) =>
    run(
      'select',
      knowledgeEmbeddingApi.select({
        provider,
        ...(provider === 'local' ? { local_model: s.local_model } : {}),
        ...(isCloud(provider)
          ? { cloud_model: cloudModel.trim(), cloud_account: cloudAccount.trim(), acknowledge_disclosure: ack }
          : {}),
      }),
      t('kb.embedding.saved'),
    ).then(() => setDisclose(false));
  const target = isCloud(provider) ? disclosureTarget(s, provider, cloudModel) : null;

  return (
    <div className="space-y-5">
      <StatusLine s={s} />

      <section className="space-y-2">
        <h3 className="text-sm font-medium text-foreground">{t('kb.embedding.provider.title')}</h3>
        <Segmented<KbEmbeddingProvider>
          aria-label={t('kb.embedding.provider.title')}
          value={provider}
          onValueChange={(v) => admin && setProvider(v)}
          options={[
            { value: 'local', label: t('kb.embedding.provider.local'), disabled: !admin },
            { value: 'openai', label: 'OpenAI', disabled: !admin },
            { value: 'gemini', label: 'Google Gemini', disabled: !admin },
            { value: 'none', label: t('kb.embedding.provider.none'), disabled: !admin },
          ]}
        />
        <p className="text-xs text-muted-foreground">{t(`kb.embedding.provider.${provider}.desc`)}</p>
        {isCloud(provider) && (
          <div className="grid gap-2 sm:grid-cols-2">
            <label className="space-y-1 text-xs text-muted-foreground">
              <span>{t('kb.embedding.cloud.model')}</span>
              <Input
                value={cloudModel}
                placeholder={target?.model}
                disabled={!admin}
                onChange={(e) => setCloudModel(e.target.value)}
              />
            </label>
            <label className="space-y-1 text-xs text-muted-foreground">
              <span>{t('kb.embedding.cloud.account')}</span>
              <Input value={cloudAccount} disabled={!admin} onChange={(e) => setCloudAccount(e.target.value)} />
            </label>
          </div>
        )}
        {admin && (
          <Button
            size="sm"
            disabled={busy !== null}
            onClick={() => (isCloud(provider) ? setDisclose(true) : void save(false))}
          >
            {t('common.save')}
          </Button>
        )}
      </section>

      {provider === 'local' && (
        <section className="space-y-2">
          <h3 className="text-sm font-medium text-foreground">{t('kb.embedding.models.title')}</h3>
          {!s.platform_supported && <p className="text-sm text-warning">{t('kb.embedding.platformUnsupported')}</p>}
          <ul className="space-y-2">
            {s.models.map((m) => (
              <ModelRow
                key={m.id}
                m={m}
                s={s}
                admin={admin}
                busy={busy}
                onInstall={() => run('install', knowledgeEmbeddingApi.install(m.id), t('kb.embedding.install.started'))}
                onCancel={() => run('cancel', knowledgeEmbeddingApi.cancel())}
                onRemove={() => run('remove', knowledgeEmbeddingApi.remove(m.id), t('kb.embedding.removed'))}
                onVerify={() => run('verify', knowledgeEmbeddingApi.verifyOffline(m.id), t('kb.embedding.verified'))}
                onUse={() =>
                  run('select', knowledgeEmbeddingApi.select({ provider: 'local', local_model: m.id }), t('kb.embedding.saved'))
                }
              />
            ))}
          </ul>
          <p className="text-xs text-muted-foreground">{t('kb.embedding.offline.hint')}</p>
        </section>
      )}

      {admin && s.available && (
        <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => run('rebuild', knowledgeEmbeddingApi.rebuild(), t('kb.embedding.rebuild.queued'))}>
          <RefreshCw aria-hidden />
          {t('kb.embedding.rebuild')}
        </Button>
      )}

      {target && (
        <EmbeddingDisclosureDialog
          open={disclose}
          vendor={target.vendor}
          host={target.host}
          model={target.model}
          redactionReady={s.redaction?.ready ?? false}
          redactionReason={s.redaction?.reason}
          busy={busy !== null}
          onConfirm={() => void save(true)}
          onClose={() => setDisclose(false)}
        />
      )}
    </div>
  );
}

function StatusLine({ s }: { s: KbEmbeddingSettings }) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  return (
    <div role="status" className="space-y-1 rounded-lg border border-surface-border bg-muted/40 px-3 py-2.5 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <Badge className={s.available ? 'bg-success/10 text-success' : 'bg-muted text-muted-foreground'}>
          {s.available ? t('kb.embedding.mode.hybrid') : t('kb.embedding.mode.keywordOnly')}
        </Badge>
        {s.available && s.egress === 'cloud' && s.cloud?.vendor && (
          <span className="text-xs text-muted-foreground">{t('kb.embedding.computedBy', { vendor: s.cloud.vendor, host: s.cloud.host ?? '' })}</span>
        )}
        {s.available && s.egress === 'local' && (
          <span className="flex items-center gap-1 text-xs text-muted-foreground">
            <ShieldCheck aria-hidden className="size-3.5" />
            {t('kb.embedding.computedLocally')}
          </span>
        )}
      </div>
      {!s.available && s.reason && <p className="text-muted-foreground">{s.reason}</p>}
      {s.available && (
        <p className="text-xs text-muted-foreground">
          {t('kb.embedding.coverage', { percent: percent(s.coverage), done: s.chunks_with_vectors, total: s.chunks_total })}
        </p>
      )}
    </div>
  );
}

function ModelRow({
  m,
  s,
  admin,
  busy,
  onInstall,
  onCancel,
  onRemove,
  onVerify,
  onUse,
}: {
  m: KbEmbeddingModel;
  s: KbEmbeddingSettings;
  admin: boolean;
  busy: string | null;
  onInstall: () => void;
  onCancel: () => void;
  onRemove: () => void;
  onVerify: () => void;
  onUse: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values);
  const downloading = s.job.state === 'downloading' && s.job.model === m.id;
  const progress = downloading && s.job.progress ? s.job.progress.done_bytes / Math.max(1, s.job.progress.total_bytes) : 0;
  const failed = s.job.state === 'error' && s.job.model === m.id ? s.job.error : null;
  return (
    <li className="rounded-lg border border-surface-border px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm font-medium text-foreground">{m.display_name}</span>
        {m.selected && <Badge className="bg-brand/10 text-brand">{t('kb.embedding.model.selected')}</Badge>}
        <Badge className="bg-muted text-muted-foreground">{m.installed ? t('kb.embedding.model.installed') : t('kb.embedding.model.notInstalled')}</Badge>
        <span className="text-xs text-muted-foreground">
          {t(m.speed === 'slow' ? 'kb.embedding.model.slow' : 'kb.embedding.model.fast')} · {m.license} · {formatBytes(m.installed ? m.model_bytes : m.size_bytes)}
        </span>
      </div>
      {downloading && (
        <p className="mt-1 text-xs text-muted-foreground" aria-live="polite">
          {t('kb.embedding.install.progress', { percent: percent(progress) })}
        </p>
      )}
      {failed && <p className="mt-1 text-xs text-destructive">{failed}</p>}
      {admin && (
        <div className="mt-2 flex flex-wrap gap-2">
          {downloading ? (
            <Button size="xs" variant="outline" onClick={onCancel}>
              {t('common.cancel')}
            </Button>
          ) : m.installed ? (
            <>
              {!m.selected && (
                <Button size="xs" disabled={busy !== null} onClick={onUse}>
                  {t('kb.embedding.model.use')}
                </Button>
              )}
              <Button size="xs" variant="destructive" disabled={busy !== null} onClick={onRemove}>
                <Trash2 aria-hidden />
                {t('kb.embedding.model.remove')}
              </Button>
            </>
          ) : (
            <>
              <Button size="xs" disabled={busy !== null || !s.platform_supported} onClick={onInstall}>
                <Download aria-hidden />
                {t('kb.embedding.model.install', { size: formatBytes(m.size_bytes) })}
              </Button>
              <Button size="xs" variant="outline" disabled={busy !== null || !s.platform_supported} onClick={onVerify}>
                {t('kb.embedding.model.verifyOffline')}
              </Button>
            </>
          )}
        </div>
      )}
    </li>
  );
}
