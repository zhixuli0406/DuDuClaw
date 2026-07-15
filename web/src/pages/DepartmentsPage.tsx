import { useCallback, useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { api, type DepartmentInfo } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import { Network, Plus, Trash2 } from 'lucide-react';
import { ConfirmDialog } from '@/components/settings/controls';
import {
  Page,
  PageHeader,
  Card,
  Badge,
  Button,
  EmptyState,
  controlClass,
} from '@/components/ui';

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
  const [newName, setNewName] = useState('');
  const [creating, setCreating] = useState(false);
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

  const validName = /^[A-Za-z0-9_-]{1,64}$/.test(newName.trim());

  const handleCreate = async () => {
    const name = newName.trim();
    if (!validName || creating) return;
    setCreating(true);
    try {
      await api.departments.create(name);
      toast.success(intl.formatMessage({ id: 'departments.created' }, { name }));
      setNewName('');
      await fetchDepartments();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setCreating(false);
    }
  };

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
    <Page>
      <PageHeader
        icon={Network}
        title={intl.formatMessage({ id: 'departments.title' })}
        subtitle={intl.formatMessage({ id: 'departments.desc' })}
      />

      <Card className="space-y-4">
        {/* Create row */}
        <div className="flex items-end gap-2">
          <div className="flex-1">
            <label className="mb-1 block text-sm font-medium text-stone-700 dark:text-stone-300">
              {intl.formatMessage({ id: 'departments.create.label' })}
            </label>
            <input
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void handleCreate();
              }}
              placeholder={intl.formatMessage({ id: 'departments.create.placeholder' })}
              className={controlClass}
            />
          </div>
          <Button variant="primary" onClick={handleCreate} disabled={!validName || creating}>
            <Plus className="h-4 w-4" />
            {intl.formatMessage({ id: 'departments.create' })}
          </Button>
        </div>
        <p className="text-xs text-stone-500 dark:text-stone-400">
          {intl.formatMessage({ id: 'departments.create.hint' })}
        </p>
      </Card>

      <Card>
        {loading ? (
          <p className="py-8 text-center text-sm text-stone-500">
            {intl.formatMessage({ id: 'common.loading' })}
          </p>
        ) : fetchError ? (
          <p className="py-8 text-center text-sm text-rose-600 dark:text-rose-400">{fetchError}</p>
        ) : departments.length === 0 ? (
          <EmptyState
            icon={Network}
            title={intl.formatMessage({ id: 'departments.empty' })}
            hint={intl.formatMessage({ id: 'departments.empty.desc' })}
          />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-[var(--panel-border)] text-left text-xs uppercase tracking-wide text-stone-500 dark:text-stone-400">
                  <th className="px-3 py-2">{intl.formatMessage({ id: 'departments.col.name' })}</th>
                  <th className="px-3 py-2">{intl.formatMessage({ id: 'departments.col.members' })}</th>
                  <th className="px-3 py-2">{intl.formatMessage({ id: 'departments.col.wiki' })}</th>
                  <th className="px-3 py-2">{intl.formatMessage({ id: 'departments.col.skills' })}</th>
                  <th className="px-3 py-2" />
                </tr>
              </thead>
              <tbody>
                {departments.map((d) => (
                  <tr key={d.name} className="border-b border-[var(--panel-border)] last:border-0">
                    <td className="px-3 py-2 font-medium">{d.name}</td>
                    <td className="px-3 py-2">
                      {d.agent_count > 0 ? (
                        <span title={d.members.join('、')}>
                          <Badge tone="info">{d.agent_count}</Badge>{' '}
                          <span className="text-stone-500 dark:text-stone-400">
                            {d.members.slice(0, 3).join('、')}
                            {d.members.length > 3 ? '…' : ''}
                          </span>
                        </span>
                      ) : (
                        <span className="text-stone-400">—</span>
                      )}
                    </td>
                    <td className="px-3 py-2">{d.wiki_pages}</td>
                    <td className="px-3 py-2">{d.skills}</td>
                    <td className="px-3 py-2 text-right">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setToRemove(d)}
                        disabled={d.agent_count > 0}
                        title={
                          d.agent_count > 0
                            ? intl.formatMessage({ id: 'departments.remove.blocked' })
                            : intl.formatMessage({ id: 'departments.remove' })
                        }
                        aria-label={`remove ${d.name}`}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

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
    </Page>
  );
}
