import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { useNavigate } from 'react-router';
import { api, type CustomWidgetSummary } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { hasMinRole } from '@/lib/roles';
import { toast, formatError } from '@/lib/toast';
import {
  CollectionPageHeader,
  CollectionPageState,
  Card,
  CardContent,
  Button,
  Badge,
  Tabs,
  TabsList,
  TabsTab,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from '@/components/mds';
import { CustomWidgetFrame } from '@/components/home/CustomWidgetFrame';
import {
  LayoutGrid, Plus, Code, Share2, Download, Upload, Trash2, Pencil, Copy, Check, MoreHorizontal,
} from 'lucide-react';

type GalleryTab = 'mine' | 'shared';

/**
 * Lazy live thumbnail: mounts the REAL sandboxed frame (bridge and all) at
 * 50% scale only once the card scrolls into view, so a large gallery doesn't
 * spawn dozens of iframes up front. Non-interactive by design.
 */
function WidgetThumb({ widgetId }: { widgetId: string }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const el = ref.current;
    if (!el || visible) return;
    const obs = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setVisible(true);
          obs.disconnect();
        }
      },
      { rootMargin: '100px' },
    );
    obs.observe(el);
    return () => obs.disconnect();
  }, [visible]);

  return (
    <div
      ref={ref}
      aria-hidden
      className="pointer-events-none h-28 select-none overflow-hidden rounded-lg border border-surface-border bg-muted/40"
    >
      {visible && (
        <div style={{ width: '200%', transform: 'scale(0.5)', transformOrigin: 'top left' }}>
          <CustomWidgetFrame widgetId={widgetId} />
        </div>
      )}
    </div>
  );
}

/** Exported/imported widget file shape (version-gated). */
interface WidgetExportFile {
  version: 1;
  title: string;
  description?: string;
  html: string;
}

/**
 * `/widgets` — Widget 工坊: my widgets + the instance-shared gallery, re-skinned
 * onto MDS. Authoring entries: guided NL flow (everyone) and raw HTML (admin).
 * Import/export moves widgets across instances — the distributor workflow. Same
 * `widgets.custom.*` / `dashboard.*` RPCs; only the surface changed.
 */
export function WidgetsPage() {
  const intl = useIntl();
  const navigate = useNavigate();
  const user = useAuthStore((s) => s.user);
  const isAdmin = hasMinRole(user?.role, 'admin');
  const [tab, setTab] = useState<GalleryTab>('mine');
  const [widgets, setWidgets] = useState<CustomWidgetSummary[]>([]);
  const [maxPerUser, setMaxPerUser] = useState(0);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [onBoard, setOnBoard] = useState<Set<string>>(new Set());
  const importRef = useRef<HTMLInputElement | null>(null);

  const refresh = useCallback(() => {
    setLoading(true);
    Promise.all([
      api.widgetsCustom.list(),
      api.dashboard.layoutGet().catch(() => ({ layout: null })),
    ])
      .then(([r, l]) => {
        setWidgets(r.widgets);
        setMaxPerUser(r.max_per_user);
        setOnBoard(new Set((l.layout?.widgets ?? []).map((w) => w.id)));
      })
      .catch((e) => toast.error(formatError(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const mine = useMemo(
    () => widgets.filter((w) => w.created_by_user === user?.id),
    [widgets, user?.id],
  );
  const shared = useMemo(
    () => widgets.filter((w) => w.shared && w.created_by_user !== user?.id),
    [widgets, user?.id],
  );
  const rows = tab === 'mine' ? mine : shared;

  /** Append `custom:<id>` to my saved layout (visible) if not present. */
  const addToBoard = async (w: CustomWidgetSummary) => {
    setBusyId(w.id);
    try {
      const lid = `custom:${w.id}`;
      const saved = await api.dashboard.layoutGet();
      const cur = saved.layout?.widgets ?? [];
      if (!cur.some((x) => x.id === lid)) {
        await api.dashboard.layoutSet([...cur, { id: lid, hidden: false }]);
      }
      setOnBoard((s) => new Set(s).add(lid));
      toast.success(intl.formatMessage({ id: 'widgets.action.added' }));
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusyId(null);
    }
  };

  const toggleShare = async (w: CustomWidgetSummary) => {
    setBusyId(w.id);
    try {
      await api.widgetsCustom.share(w.id, !w.shared);
      refresh();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusyId(null);
    }
  };

  const remove = async (w: CustomWidgetSummary) => {
    if (!window.confirm(intl.formatMessage({ id: 'widgets.action.deleteConfirm' }, { title: w.title }))) return;
    setBusyId(w.id);
    try {
      await api.widgetsCustom.remove(w.id);
      refresh();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusyId(null);
    }
  };

  const exportWidget = async (w: CustomWidgetSummary) => {
    try {
      const full = await api.widgetsCustom.get(w.id);
      const file: WidgetExportFile = {
        version: 1,
        title: full.title,
        description: full.description,
        html: full.html,
      };
      const blob = new Blob([JSON.stringify(file, null, 2)], { type: 'application/json' });
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = `duduclaw-widget-${w.title.replace(/[^\w一-鿿-]+/g, '_')}.json`;
      a.click();
      URL.revokeObjectURL(a.href);
    } catch (e) {
      toast.error(formatError(e));
    }
  };

  /** Duplicate a shared widget into my own collection (then I can regenerate). */
  const copyToMine = async (w: CustomWidgetSummary) => {
    setBusyId(w.id);
    try {
      const full = await api.widgetsCustom.get(w.id);
      await api.widgetsCustom.create({
        title: intl.formatMessage({ id: 'widgets.copy.title' }, { title: full.title }),
        description: full.description,
        html: full.html,
        origin: 'ai',
      });
      setTab('mine');
      refresh();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusyId(null);
    }
  };

  const onImportFile = async (file: File) => {
    try {
      const parsed = JSON.parse(await file.text()) as Partial<WidgetExportFile>;
      if (parsed.version !== 1 || typeof parsed.title !== 'string' || typeof parsed.html !== 'string') {
        toast.error(intl.formatMessage({ id: 'widgets.import.invalid' }));
        return;
      }
      await api.widgetsCustom.create({
        title: parsed.title,
        description: parsed.description ?? '',
        html: parsed.html,
        // Origin drives the reopening edit surface; only admins own the raw
        // HTML surface, so a non-admin import lands as an AI-origin widget.
        origin: isAdmin ? 'html' : 'ai',
      });
      toast.success(intl.formatMessage({ id: 'widgets.import.done' }));
      setTab('mine');
      refresh();
    } catch (e) {
      toast.error(formatError(e));
    }
  };

  return (
    <div className="-mx-4 -mt-4 flex flex-col md:-mx-6 md:-mt-6">
      <CollectionPageHeader
        hideTrigger
        icon={LayoutGrid}
        title={intl.formatMessage({ id: 'widgets.title' })}
        count={rows.length || undefined}
        action={
          <div className="flex items-center gap-2">
            <input
              ref={importRef}
              type="file"
              accept="application/json"
              className="hidden"
              onChange={(e) => {
                const f = e.target.files?.[0];
                if (f) void onImportFile(f);
                e.target.value = '';
              }}
            />
            <Button variant="ghost" size="sm" onClick={() => importRef.current?.click()}>
              <Upload />
              <span className="hidden sm:inline">{intl.formatMessage({ id: 'widgets.import' })}</span>
            </Button>
            {isAdmin && (
              <Button variant="outline" size="sm" onClick={() => navigate('/widgets/new?mode=html')}>
                <Code />
                <span className="hidden sm:inline">{intl.formatMessage({ id: 'widgets.newHtml' })}</span>
              </Button>
            )}
            <Button variant="brand" size="sm" onClick={() => navigate('/widgets/new')}>
              <Plus />
              <span className="hidden sm:inline">{intl.formatMessage({ id: 'widgets.new' })}</span>
            </Button>
          </div>
        }
      />

      {/* Gallery switcher (line tabs). */}
      <div className="flex h-12 shrink-0 items-center gap-2 overflow-x-auto border-b border-surface-border px-4">
        <Tabs variant="line" value={tab} onValueChange={(v) => setTab(v as GalleryTab)}>
          <TabsList>
            <TabsTab value="mine">
              {intl.formatMessage(
                { id: maxPerUser > 0 ? 'widgets.tab.mineCapped' : 'widgets.tab.mine' },
                { count: mine.length, cap: maxPerUser },
              )}
            </TabsTab>
            <TabsTab value="shared">
              {intl.formatMessage({ id: 'widgets.tab.shared' }, { count: shared.length })}
            </TabsTab>
          </TabsList>
        </Tabs>
      </div>

      <div className="flex flex-1 flex-col p-4 md:p-6">
        {loading ? (
          <CollectionPageState state="loading" />
        ) : rows.length === 0 ? (
          <CollectionPageState
            state="empty"
            icon={LayoutGrid}
            title={intl.formatMessage({ id: tab === 'mine' ? 'widgets.empty.mine' : 'widgets.empty.shared' })}
            action={
              tab === 'mine' ? (
                <Button variant="brand" size="sm" onClick={() => navigate('/widgets/new')}>
                  <Plus />
                  {intl.formatMessage({ id: 'widgets.new' })}
                </Button>
              ) : undefined
            }
          />
        ) : (
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {rows.map((w) => {
              const isMine = w.created_by_user === user?.id;
              const busy = busyId === w.id;
              const placed = onBoard.has(`custom:${w.id}`);
              return (
                <Card key={w.id} className="gap-3">
                  <CardContent className="space-y-3">
                    <WidgetThumb widgetId={w.id} />
                    <div className="flex items-start justify-between gap-2">
                      <div className="min-w-0">
                        <p className="truncate text-sm font-medium text-foreground" title={w.title}>
                          {w.title}
                        </p>
                        <p className="mt-0.5 line-clamp-2 text-sm text-muted-foreground">
                          {w.description || intl.formatMessage({ id: 'widgets.noDescription' })}
                        </p>
                      </div>
                      <Badge variant={w.origin === 'ai' ? 'default' : 'secondary'}>
                        {intl.formatMessage({ id: `widgets.badge.${w.origin}` })}
                      </Badge>
                    </div>
                    <div className="flex items-center justify-between gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy || placed}
                        onClick={() => void addToBoard(w)}
                      >
                        {placed ? <Check /> : <Plus />}
                        {intl.formatMessage({ id: placed ? 'widgets.action.onBoard' : 'widgets.action.addToBoard' })}
                      </Button>
                      <DropdownMenu>
                        <DropdownMenuTrigger
                          render={
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              disabled={busy}
                              aria-label={intl.formatMessage({ id: 'widgets.moreActions' })}
                            />
                          }
                        >
                          <MoreHorizontal />
                        </DropdownMenuTrigger>
                        <DropdownMenuContent>
                          {isMine && (
                            <>
                              <DropdownMenuItem onClick={() => navigate(`/widgets/${w.id}/edit`)}>
                                <Pencil />
                                {intl.formatMessage({ id: 'common.edit' })}
                              </DropdownMenuItem>
                              <DropdownMenuItem onClick={() => void toggleShare(w)}>
                                <Share2 />
                                {intl.formatMessage({ id: w.shared ? 'widgets.action.unshare' : 'widgets.action.share' })}
                              </DropdownMenuItem>
                            </>
                          )}
                          {!isMine && (
                            <DropdownMenuItem onClick={() => void copyToMine(w)}>
                              <Copy />
                              {intl.formatMessage({ id: 'widgets.action.copy' })}
                            </DropdownMenuItem>
                          )}
                          <DropdownMenuItem onClick={() => void exportWidget(w)}>
                            <Download />
                            {intl.formatMessage({ id: 'widgets.action.export' })}
                          </DropdownMenuItem>
                          {(isMine || isAdmin) && (
                            <DropdownMenuItem variant="destructive" onClick={() => void remove(w)}>
                              <Trash2 />
                              {intl.formatMessage({ id: 'common.delete' })}
                            </DropdownMenuItem>
                          )}
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </div>
                  </CardContent>
                </Card>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
