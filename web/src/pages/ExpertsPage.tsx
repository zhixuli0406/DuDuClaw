import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { api, type ExpertPack, type ExpertCatalogEntry } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { toast, formatError } from '@/lib/toast';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  CardContent,
  Button,
  Badge,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  Spinner,
} from '@/components/mds';
import {
  Package,
  Upload,
  Trash2,
  Users,
  Puzzle,
  BookOpen,
  ShieldCheck,
  ArrowRight,
  Download,
  Check,
  Sparkles,
  Building2,
} from 'lucide-react';
import { GenerateExpertDialog } from '@/components/experts/GenerateExpertDialog';

/**
 * ExpertsPage — 專家包 management (admin-only, mirrors the `experts.*` RPCs).
 *
 * A pack bundles a ready-made AI team: staffers, skills, and knowledge pages.
 * Flow: upload a .zip → `POST /api/experts/upload` stages it server-side →
 * `experts.install` runs the full security-scanned install pipeline. Packs
 * that ship hooks land with hooks DISABLED pending approval (fail-closed);
 * once decided in the inbox/approval center, the "套用" button applies the
 * decision via `experts.hooks_apply`.
 */
export function ExpertsPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const t = useCallback((id: string, values?: Record<string, string | number>) => intl.formatMessage({ id }, values), [intl]);
  const [packs, setPacks] = useState<ExpertPack[] | null>(null);
  const [loadError, setLoadError] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [uploadPct, setUploadPct] = useState<number | null>(null);
  const [removeTarget, setRemoveTarget] = useState<ExpertPack | null>(null);
  const [removing, setRemoving] = useState(false);
  const [applyingHooks, setApplyingHooks] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [catalog, setCatalog] = useState<{
    deployed: boolean;
    present_but_locked: boolean;
    packs: ExpertCatalogEntry[];
  } | null>(null);
  const [installingBuiltin, setInstallingBuiltin] = useState<string | null>(null);
  const [generateOpen, setGenerateOpen] = useState(false);

  const load = useCallback(async () => {
    try {
      const res = await api.experts.list();
      setPacks(res.packs ?? []);
      setLoadError(false);
    } catch (e) {
      setLoadError(true);
      toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
    }
    // Catalog is best-effort — a failure never blocks the installed list.
    try {
      const cat = await api.experts.catalog();
      setCatalog({
        deployed: cat.deployed,
        present_but_locked: cat.present_but_locked,
        packs: cat.packs ?? [],
      });
    } catch {
      setCatalog(null);
    }
  }, [intl]);

  useEffect(() => { load(); }, [load]);

  const handleInstallBuiltin = async (entry: ExpertCatalogEntry) => {
    setInstallingBuiltin(entry.industry);
    try {
      await api.experts.installBuiltin(entry.industry);
      toast.success(t('experts.install.success'));
      await load();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'experts.install.failed' }, { message: formatError(e) }));
    } finally {
      setInstallingBuiltin(null);
    }
  };

  /** Upload the .zip with progress (XHR — fetch has no upload progress). */
  const uploadZip = (file: File): Promise<string> =>
    new Promise((resolve, reject) => {
      const jwt = useAuthStore.getState().jwt;
      const xhr = new XMLHttpRequest();
      xhr.open('POST', '/api/experts/upload');
      if (jwt) xhr.setRequestHeader('Authorization', `Bearer ${jwt}`);
      xhr.upload.onprogress = (ev) => {
        if (ev.lengthComputable) setUploadPct(Math.round((ev.loaded / ev.total) * 100));
      };
      xhr.onload = () => {
        try {
          const body = JSON.parse(xhr.responseText || '{}');
          if (xhr.status >= 200 && xhr.status < 300 && typeof body.path === 'string') {
            resolve(body.path);
          } else {
            reject(new Error(typeof body.error === 'string' ? body.error : `HTTP ${xhr.status}`));
          }
        } catch {
          reject(new Error(`HTTP ${xhr.status}`));
        }
      };
      xhr.onerror = () => reject(new Error('network error'));
      const form = new FormData();
      form.append('file', file, file.name);
      xhr.send(form);
    });

  const handleFilePicked = async (file: File | null) => {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith('.zip')) {
      toast.error(t('experts.upload.notZip'));
      return;
    }
    if (file.size > 50 * 1024 * 1024) {
      toast.error(t('experts.upload.tooLarge'));
      return;
    }
    setInstalling(true);
    setUploadPct(0);
    try {
      const path = await uploadZip(file);
      setUploadPct(null); // upload done — now the install/scan phase
      await api.experts.install(path);
      toast.success(t('experts.install.success'));
      await load();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'experts.install.failed' }, { message: formatError(e) }));
    } finally {
      setInstalling(false);
      setUploadPct(null);
      if (fileInputRef.current) fileInputRef.current.value = '';
    }
  };

  const handleRemove = async () => {
    if (!removeTarget) return;
    setRemoving(true);
    try {
      await api.experts.remove(removeTarget.slug);
      toast.success(t('experts.remove.success'));
      setRemoveTarget(null);
      await load();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setRemoving(false);
    }
  };

  const handleApplyHooks = async (slug: string) => {
    setApplyingHooks(slug);
    try {
      const res = await api.experts.hooksApply(slug);
      if (res.status === 'enabled') {
        toast.success(t('experts.hooks.applied'));
      } else if (res.status === 'pending_approval') {
        toast.info(t('experts.hooks.stillPending'));
      } else {
        // denied / expired / disabled — fail-closed, stays off.
        toast.info(t('experts.hooks.keptDisabled'));
      }
      await load();
    } catch (e) {
      toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
    } finally {
      setApplyingHooks(null);
    }
  };

  const hooksBadge = (p: ExpertPack) => {
    switch (p.hooks_status) {
      case 'enabled':
        return <Badge variant="secondary"><ShieldCheck />{t('experts.hooks.enabled')}</Badge>;
      case 'pending_approval':
        return <Badge variant="destructive">{t('experts.hooks.pending')}</Badge>;
      case 'disabled':
        return <Badge variant="ghost">{t('experts.hooks.disabled')}</Badge>;
      default:
        return null;
    }
  };

  const uploadAction = (
    <Button variant="brand" size="sm" disabled={installing} onClick={() => fileInputRef.current?.click()}>
      {installing ? (
        <>
          <Spinner className="size-3.5" />
          {uploadPct !== null
            ? intl.formatMessage({ id: 'experts.uploading' }, { pct: uploadPct })
            : t('experts.installing')}
        </>
      ) : (
        <>
          <Upload />
          {t('experts.upload')}
        </>
      )}
    </Button>
  );

  const headerActions = (
    <div className="flex items-center gap-2">
      <Button variant="outline" size="sm" onClick={() => setGenerateOpen(true)}>
        <Sparkles />
        {t('experts.generate.open')}
      </Button>
      {uploadAction}
    </div>
  );

  const builtinSection = (
    <section className="mt-8 space-y-3">
      <div>
        <h2 className="flex items-center gap-2 text-sm font-medium">
          <Building2 className="size-4 text-muted-foreground" />
          {t('experts.builtin.title')}
        </h2>
        <p className="mt-0.5 text-xs text-muted-foreground">{t('experts.builtin.desc')}</p>
      </div>
      {catalog === null || !catalog.deployed || catalog.present_but_locked ? (
        <div className="rounded-xl border border-dashed border-surface-border px-4 py-6 text-center">
          <p className="text-sm text-muted-foreground">
            {catalog?.present_but_locked
              ? t('experts.builtin.locked')
              : t('experts.builtin.notDeployed')}
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {catalog.packs.map((entry) => (
            <Card key={entry.industry} className="gap-3">
              <CardContent className="space-y-3">
                <div className="min-w-0">
                  <h3 className="truncate text-sm font-medium">{entry.label}</h3>
                  {entry.description && (
                    <p className="mt-1 line-clamp-2 text-xs text-muted-foreground">
                      {entry.description}
                    </p>
                  )}
                </div>
                <div className="flex items-center justify-between">
                  <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
                    <Users className="size-3.5" />
                    {intl.formatMessage({ id: 'experts.card.agents' }, { count: entry.agents_count })}
                  </span>
                  {entry.installed ? (
                    <Badge variant="secondary">
                      <Check />
                      {t('experts.builtin.installed')}
                    </Badge>
                  ) : (
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={installingBuiltin !== null}
                      onClick={() => handleInstallBuiltin(entry)}
                    >
                      {installingBuiltin === entry.industry ? (
                        <>
                          <Spinner className="size-3.5" />
                          {t('experts.installing')}
                        </>
                      ) : (
                        <>
                          <Download />
                          {t('experts.builtin.install')}
                        </>
                      )}
                    </Button>
                  )}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </section>
  );

  return (
    <div className="flex h-full flex-col">
      <input
        ref={fileInputRef}
        type="file"
        accept=".zip"
        className="hidden"
        onChange={(e) => handleFilePicked(e.target.files?.[0] ?? null)}
      />
      <CollectionPageHeader
        icon={Package}
        title={t('experts.title')}
        count={packs?.length}
        description={t('experts.subtitle')}
        action={headerActions}
      />

      <div className="flex-1 overflow-y-auto p-5">
        {packs === null ? (
          <CollectionPageState state={loadError ? 'error' : 'loading'} title={t('experts.title')} />
        ) : packs.length === 0 ? (
          <>
            <CollectionPageState
              state="empty"
              icon={Package}
              title={t('experts.empty.title')}
              description={t('experts.empty.desc')}
              action={uploadAction}
            />
            {builtinSection}
          </>
        ) : (
          <>
          <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
            {packs.map((p) => (
              <Card key={p.slug} className="gap-3">
                <CardContent className="space-y-3">
                  <div className="flex items-start justify-between gap-2">
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <h3 className="truncate text-sm font-medium">{p.display_name}</h3>
                        <span className="shrink-0 font-mono text-xs text-muted-foreground">v{p.version || '0.0.0'}</span>
                      </div>
                      {p.description && (
                        <p className="mt-1 line-clamp-2 text-xs text-muted-foreground">{p.description}</p>
                      )}
                    </div>
                    {hooksBadge(p)}
                  </div>

                  <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
                    <span className="inline-flex items-center gap-1">
                      <Users className="size-3.5" />
                      {intl.formatMessage({ id: 'experts.card.agents' }, { count: p.agents.length })}
                    </span>
                    <span className="inline-flex items-center gap-1">
                      <Puzzle className="size-3.5" />
                      {intl.formatMessage({ id: 'experts.card.skills' }, { count: p.skills_count })}
                    </span>
                    <span className="inline-flex items-center gap-1">
                      <BookOpen className="size-3.5" />
                      {intl.formatMessage({ id: 'experts.card.wiki' }, { count: p.wiki_count })}
                    </span>
                  </div>

                  {p.hooks_status === 'pending_approval' && (
                    <div className="space-y-2 rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs">
                      <p>{t('experts.hooks.pendingHint')}</p>
                      <div className="flex items-center gap-2">
                        <Button variant="outline" size="sm" onClick={() => navigate('/inbox')}>
                          {t('experts.hooks.goApprove')}
                          <ArrowRight />
                        </Button>
                        <Button
                          variant="secondary"
                          size="sm"
                          disabled={applyingHooks === p.slug}
                          onClick={() => handleApplyHooks(p.slug)}
                        >
                          {applyingHooks === p.slug && <Spinner className="size-3.5" />}
                          {t('experts.hooks.apply')}
                        </Button>
                      </div>
                    </div>
                  )}

                  <div className="flex items-center justify-end">
                    <Button variant="ghost" size="sm" onClick={() => setRemoveTarget(p)}>
                      <Trash2 />
                      {t('experts.remove')}
                    </Button>
                  </div>
                </CardContent>
              </Card>
            ))}
          </div>
          {builtinSection}
          </>
        )}
      </div>

      <GenerateExpertDialog
        open={generateOpen}
        onOpenChange={setGenerateOpen}
        onInstalled={() => { load(); }}
      />

      <Dialog open={removeTarget !== null} onOpenChange={(open) => { if (!open) setRemoveTarget(null); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('experts.remove.confirmTitle')}</DialogTitle>
            <DialogDescription>
              {intl.formatMessage(
                { id: 'experts.remove.confirmDesc' },
                { name: removeTarget?.display_name ?? '', count: removeTarget?.agents.length ?? 0 },
              )}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setRemoveTarget(null)} disabled={removing}>
              {t('common.cancel')}
            </Button>
            <Button variant="destructive" size="sm" onClick={handleRemove} disabled={removing}>
              {removing && <Spinner className="size-3.5" />}
              {t('experts.remove')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
