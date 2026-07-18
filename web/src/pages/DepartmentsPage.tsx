import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type DepartmentInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Network, Plus, Trash2, MoreHorizontal, Loader2 } from 'lucide-react';
import { ConfirmDialog } from '@/components/settings/controls';
import {
  Button,
  Badge,
  Input,
  Empty,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
} from '@/components/mds';

const COLUMNS = 'minmax(0,1.4fr) minmax(0,1.6fr) minmax(0,0.6fr) minmax(0,0.6fr) 2.5rem';

/**
 * Mirror of `duduclaw_core::is_valid_department` (Bug#5): 1..=64 bytes, not
 * `.`/`..`, and no path separator / whitespace / control char. Allows CJK and
 * other printable Unicode so a zh-TW department like "測試部" is valid, while a
 * filesystem path built from the name can never traverse.
 */
export function isValidDepartmentName(name: string): boolean {
  if (name === '' || name === '.' || name === '..') return false;
  if (new TextEncoder().encode(name).length > 64) return false;
  // Reject path separators, whitespace, and control characters (C0 range +
  // DEL). \s covers the ASCII space, tab and newline.
  // eslint-disable-next-line no-control-regex
  return !/[/\\\s\u0000-\u001f\u007f]/.test(name);
}

/**
 * 部門管理 — pre-create departments so the create-agent dialog offers them as
 * a dropdown (WP7 derived design: a department is its agents + its shared
 * wiki/skill sub-trees; creating one here materialises the wiki directory).
 */
export function DepartmentsPage() {
  const intl = useIntl();
  const [departments, setDepartments] = useState<DepartmentInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [toRemove, setToRemove] = useState<DepartmentInfo | null>(null);

  const fetchDepartments = useCallback(async () => {
    try {
      setFetchError(null);
      const { departments: data } = await api.departments.list();
      setDepartments(data);
    } catch (e) {
      setFetchError(e instanceof Error ? e.message : 'Failed to load departments');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void fetchDepartments();
  }, [fetchDepartments]);

  const handleRemove = async (dept: DepartmentInfo) => {
    try {
      // Content exists ⇒ the confirm dialog already spelled out that the wiki
      // pages / skills go with it, so pass force.
      await api.departments.remove(dept.name, dept.wiki_pages > 0 || dept.skills > 0);
      toast.success(intl.formatMessage({ id: 'departments.removed' }, { name: dept.name }));
      await fetchDepartments();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setToRemove(null);
    }
  };

  return (
    <div className="mx-auto w-full max-w-[1200px] space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-2">
          <Network className="size-5 text-muted-foreground" />
          <div>
            <h1 className="text-base font-medium">{intl.formatMessage({ id: 'departments.title' })}</h1>
            <p className="text-sm text-muted-foreground">{intl.formatMessage({ id: 'departments.desc' })}</p>
          </div>
        </div>
        <div className="flex gap-2">
          <Button variant="brand" size="sm" onClick={() => setShowCreate(true)}>
            <Plus />
            <span className="hidden sm:inline">{intl.formatMessage({ id: 'departments.create' })}</span>
          </Button>
        </div>
      </div>

      {fetchError && (
        <div className="rounded-lg bg-destructive/10 px-4 py-3 text-sm text-destructive">{fetchError}</div>
      )}

      {loading ? (
        <div className="flex items-center justify-center py-16">
          <Loader2 className="size-6 animate-spin text-muted-foreground" />
        </div>
      ) : departments.length === 0 ? (
        <Empty
          icon={Network}
          title={intl.formatMessage({ id: 'departments.empty' })}
          description={intl.formatMessage({ id: 'departments.empty.desc' })}
        />
      ) : (
        <div className="overflow-hidden rounded-xl border border-surface-border">
          <ListGridContainer
            columns={COLUMNS}
            className="!h-auto [&>[aria-hidden]]:hidden"
            header={
              <ListGridHeader>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'departments.col.name' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'departments.col.members' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'departments.col.wiki' })}</ListGridHeaderCell>
                <ListGridHeaderCell>{intl.formatMessage({ id: 'departments.col.skills' })}</ListGridHeaderCell>
                <ListGridHeaderCell aria-hidden />
              </ListGridHeader>
            }
          >
            {departments.map((d) => (
              <ListGridRow key={d.name} className="cursor-default">
                <ListGridCell>
                  <span className="truncate text-sm font-medium text-foreground">{d.name}</span>
                </ListGridCell>
                <ListGridCell className="gap-1.5">
                  {d.agent_count > 0 ? (
                    <span className="flex min-w-0 items-center gap-1.5" title={d.members.join('、')}>
                      <Badge variant="secondary">{d.agent_count}</Badge>
                      <span className="truncate text-xs text-muted-foreground">
                        {d.members.slice(0, 3).join('、')}
                        {d.members.length > 3 ? '…' : ''}
                      </span>
                    </span>
                  ) : (
                    <span className="text-xs text-muted-foreground">—</span>
                  )}
                </ListGridCell>
                <ListGridCell>
                  <span className="font-mono text-xs tabular-nums text-muted-foreground">{d.wiki_pages}</span>
                </ListGridCell>
                <ListGridCell>
                  <span className="font-mono text-xs tabular-nums text-muted-foreground">{d.skills}</span>
                </ListGridCell>
                <ListGridCell className="justify-end">
                  <DropdownMenu>
                    <DropdownMenuTrigger
                      render={
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label={intl.formatMessage({ id: 'common.more' })}
                          data-stop-row-nav
                        />
                      }
                    >
                      <MoreHorizontal />
                    </DropdownMenuTrigger>
                    <DropdownMenuContent>
                      <DropdownMenuItem
                        variant="destructive"
                        disabled={d.agent_count > 0}
                        title={
                          d.agent_count > 0
                            ? intl.formatMessage({ id: 'departments.remove.blocked' })
                            : intl.formatMessage({ id: 'departments.remove' })
                        }
                        onClick={() => setToRemove(d)}
                      >
                        <Trash2 />
                        {intl.formatMessage({ id: 'departments.remove' })}
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </ListGridCell>
              </ListGridRow>
            ))}
          </ListGridContainer>
        </div>
      )}

      {/* Create Department Dialog */}
      {showCreate && (
        <CreateDepartmentDialog
          onClose={() => setShowCreate(false)}
          onCreated={() => {
            setShowCreate(false);
            fetchDepartments();
          }}
        />
      )}

      {toRemove && (
        <ConfirmDialog
          open
          title={intl.formatMessage({ id: 'departments.remove' })}
          message={
            toRemove.wiki_pages > 0 || toRemove.skills > 0
              ? intl.formatMessage(
                  { id: 'departments.remove.confirmWithContent' },
                  { name: toRemove.name, wiki: toRemove.wiki_pages, skills: toRemove.skills },
                )
              : intl.formatMessage({ id: 'departments.remove.confirm' }, { name: toRemove.name })
          }
          onConfirm={() => void handleRemove(toRemove)}
          onClose={() => setToRemove(null)}
        />
      )}
    </div>
  );
}

function CreateDepartmentDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const [newName, setNewName] = useState('');
  const [creating, setCreating] = useState(false);

  const validName = isValidDepartmentName(newName.trim());

  const handleCreate = async () => {
    const name = newName.trim();
    if (!validName || creating) return;
    setCreating(true);
    try {
      await api.departments.create(name);
      toast.success(intl.formatMessage({ id: 'departments.created' }, { name }));
      onCreated();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setCreating(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{intl.formatMessage({ id: 'departments.create' })}</DialogTitle>
        </DialogHeader>
        <div className="space-y-1.5">
          <label className="text-sm font-medium text-foreground">
            {intl.formatMessage({ id: 'departments.create.label' })}
          </label>
          <Input
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void handleCreate();
            }}
            placeholder={intl.formatMessage({ id: 'departments.create.placeholder' })}
            autoFocus
          />
          <p className="text-xs text-muted-foreground">{intl.formatMessage({ id: 'departments.create.hint' })}</p>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {intl.formatMessage({ id: 'common.cancel' })}
          </Button>
          <Button variant="brand" onClick={handleCreate} disabled={!validName || creating}>
            {intl.formatMessage({ id: 'departments.create' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
