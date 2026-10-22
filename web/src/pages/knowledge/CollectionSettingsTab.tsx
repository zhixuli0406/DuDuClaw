import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { Loader2, Trash2 } from 'lucide-react';
import { Button, Input, Switch, Textarea } from '@/components/mds';
import { selectClass } from '@/components/shared/controlClass';
import { ConfirmDialog } from '@/components/settings/controls/ConfirmDialog';
import { useSystemStore } from '@/stores/system-store';
import { toast, formatError } from '@/lib/toast';
import { KB_SENSITIVITIES, knowledgeApi, type KbCollection, type KbCollectionPatch, type KbSensitivity } from '@/lib/knowledge-api';

/** Only the fields that differ from the saved collection. */
export function settingsPatch(
  saved: KbCollection,
  form: { name: string; description: string; department: string; sensitivity: KbSensitivity; localOnly: boolean },
): KbCollectionPatch {
  const patch: KbCollectionPatch = {};
  if (form.name.trim() !== saved.name) patch.name = form.name.trim();
  if (form.description.trim() !== saved.description) patch.description = form.description.trim();
  if (form.department.trim() !== (saved.department ?? '')) patch.department = form.department.trim();
  if (form.sensitivity !== saved.sensitivity) patch.sensitivity = form.sensitivity;
  const policy = form.localOnly ? 'local_only' : 'inherit';
  if (policy !== saved.embedding_policy) patch.embedding_policy = policy;
  return patch;
}

/**
 * 設定 (owners): name, description, department, sensitivity, whether the
 * text may ever be sent to an outside service for meaning-based search, and
 * deleting the collection (type its name to confirm).
 */
export function CollectionSettingsTab({
  collection,
  onSaved,
  onDeleted,
}: {
  collection: KbCollection;
  onSaved: (c: KbCollection) => void;
  onDeleted: () => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const isPersonal = useSystemStore((s) => s.status?.edition_profile) === 'personal';
  const [name, setName] = useState(collection.name);
  const [description, setDescription] = useState(collection.description);
  const [department, setDepartment] = useState(collection.department ?? '');
  const [sensitivity, setSensitivity] = useState<KbSensitivity>(collection.sensitivity);
  const [localOnly, setLocalOnly] = useState(collection.embedding_policy === 'local_only');
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);

  useEffect(() => {
    setName(collection.name);
    setDescription(collection.description);
    setDepartment(collection.department ?? '');
    setSensitivity(collection.sensitivity);
    setLocalOnly(collection.embedding_policy === 'local_only');
  }, [collection]);

  const patch = settingsPatch(collection, { name, description, department, sensitivity, localOnly });
  const dirty = Object.keys(patch).length > 0;

  const save = async () => {
    if (!dirty || saving || name.trim() === '') return;
    setSaving(true);
    try {
      const r = await knowledgeApi.updateCollection(collection.id, collection.updated_at, patch);
      onSaved({ ...collection, ...r.collection });
      toast.success(t('kb.settings.saved'));
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    setDeleting(true);
    try {
      await knowledgeApi.deleteCollection(collection.id, collection.name);
      toast.success(t('kb.settings.deleted', { name: collection.name }));
      onDeleted();
    } catch (e) {
      toast.error(formatError(e));
      setDeleting(false);
    }
  };

  return (
    <div className="max-w-2xl space-y-6">
      <section className="space-y-4 rounded-xl border border-surface-border p-4">
        <div className="space-y-1.5">
          <label htmlFor="kb-set-name" className="text-sm font-medium">
            {t('kb.field.name')}
          </label>
          <Input id="kb-set-name" value={name} maxLength={120} onChange={(e) => setName(e.target.value)} aria-invalid={name.trim() === ''} />
        </div>
        <div className="space-y-1.5">
          <label htmlFor="kb-set-desc" className="text-sm font-medium">
            {t('kb.field.description')}
          </label>
          <Textarea id="kb-set-desc" rows={3} maxLength={2000} value={description} onChange={(e) => setDescription(e.target.value)} />
        </div>
        {!isPersonal && (
          <div className="space-y-1.5">
            <label htmlFor="kb-set-dept" className="text-sm font-medium">
              {t('kb.field.department')}
            </label>
            <Input id="kb-set-dept" value={department} maxLength={64} onChange={(e) => setDepartment(e.target.value)} />
            <p className="text-xs text-muted-foreground">{t('kb.field.departmentHint')}</p>
          </div>
        )}
        <div className="space-y-1.5">
          <label htmlFor="kb-set-sens" className="text-sm font-medium">
            {t('kb.field.sensitivity')}
          </label>
          <select
            id="kb-set-sens"
            className={selectClass}
            value={sensitivity}
            onChange={(e) => setSensitivity(e.target.value as KbSensitivity)}
          >
            {KB_SENSITIVITIES.map((s) => (
              <option key={s} value={s}>
                {t(`kb.sensitivity.${s}`)}
              </option>
            ))}
          </select>
          <p className="text-xs text-muted-foreground">{t(`kb.sensitivity.${sensitivity}.hint`)}</p>
        </div>
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <p id="kb-set-local-label" className="text-sm font-medium">
              {t('kb.field.localOnly')}
            </p>
            <p className="text-xs text-muted-foreground">{t('kb.field.localOnlyHint')}</p>
          </div>
          <Switch aria-labelledby="kb-set-local-label" checked={localOnly} onCheckedChange={(v) => setLocalOnly(Boolean(v))} />
        </div>
        <div className="flex justify-end">
          <Button variant="brand" size="sm" onClick={() => void save()} disabled={!dirty || saving || name.trim() === ''}>
            {saving && <Loader2 aria-hidden className="motion-safe:animate-spin" />}
            {t('common.save')}
          </Button>
        </div>
      </section>

      <section className="space-y-2 rounded-xl border border-destructive/30 p-4">
        <h2 className="text-sm font-medium text-destructive">{t('kb.settings.danger')}</h2>
        <p className="text-sm text-muted-foreground">{t('kb.settings.deleteDesc')}</p>
        <Button variant="destructive" size="sm" onClick={() => setConfirmDelete(true)}>
          <Trash2 aria-hidden />
          {t('kb.settings.delete')}
        </Button>
      </section>

      {confirmDelete && (
        <ConfirmDialog
          open
          title={t('kb.settings.delete')}
          message={t('kb.settings.deleteConfirm', { name: collection.name })}
          confirmLabel={t('common.delete')}
          requireText={collection.name}
          requireTextHint={t('kb.settings.deleteType', { name: collection.name })}
          busy={deleting}
          onConfirm={() => void remove()}
          onClose={() => setConfirmDelete(false)}
        />
      )}
    </div>
  );
}
