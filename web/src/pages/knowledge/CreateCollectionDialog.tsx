import { useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Textarea,
} from '@/components/mds';
import { selectClass } from '@/components/shared/controlClass';
import { isImeComposing } from '@/lib/keyboard';
import { toast, formatError } from '@/lib/toast';
import { KB_SENSITIVITIES, knowledgeApi, type KbCollection, type KbSensitivity } from '@/lib/knowledge-api';

/** Collection codes: lowercase letters, digits and dashes, 1–64 long. */
export const SLUG_RE = /^[a-z0-9][a-z0-9-]{0,63}$/;

/**
 * 新增集合. The creator becomes its owner; nobody else can see it until it
 * is shared on the 權限 tab. The code (slug) is optional — the gateway
 * derives one from the name when it is left empty.
 */
export function CreateCollectionDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (c: KbCollection) => void;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [name, setName] = useState('');
  const [slug, setSlug] = useState('');
  const [description, setDescription] = useState('');
  const [sensitivity, setSensitivity] = useState<KbSensitivity>('internal');
  const [busy, setBusy] = useState(false);

  const slugOk = slug.trim() === '' || SLUG_RE.test(slug.trim());
  const canSubmit = name.trim() !== '' && slugOk && !busy;

  const submit = async () => {
    if (!canSubmit) return;
    setBusy(true);
    try {
      const { collection } = await knowledgeApi.createCollection({
        name: name.trim(),
        ...(slug.trim() ? { slug: slug.trim() } : {}),
        description: description.trim(),
        sensitivity,
      });
      toast.success(intl.formatMessage({ id: 'kb.create.done' }, { name: collection.name }));
      onCreated(collection);
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('kb.create')}</DialogTitle>
          <DialogDescription>{t('kb.create.desc')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="kb-new-name" className="text-sm font-medium">
              {t('kb.field.name')}
            </label>
            <Input
              id="kb-new-name"
              value={name}
              autoFocus
              maxLength={120}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !isImeComposing(e)) void submit();
              }}
              placeholder={t('kb.field.namePlaceholder')}
            />
          </div>
          <div className="space-y-1.5">
            <label htmlFor="kb-new-desc" className="text-sm font-medium">
              {t('kb.field.description')}
            </label>
            <Textarea
              id="kb-new-desc"
              value={description}
              rows={2}
              maxLength={2000}
              onChange={(e) => setDescription(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <label htmlFor="kb-new-sensitivity" className="text-sm font-medium">
              {t('kb.field.sensitivity')}
            </label>
            <select
              id="kb-new-sensitivity"
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
          <div className="space-y-1.5">
            <label htmlFor="kb-new-slug" className="text-sm font-medium">
              {t('kb.field.slug')}
            </label>
            <Input
              id="kb-new-slug"
              value={slug}
              maxLength={64}
              onChange={(e) => setSlug(e.target.value.toLowerCase())}
              aria-invalid={!slugOk}
              aria-describedby="kb-new-slug-hint"
              placeholder="hr-handbook"
            />
            <p id="kb-new-slug-hint" className={slugOk ? 'text-xs text-muted-foreground' : 'text-xs text-destructive'}>
              {t('kb.field.slugHint')}
            </p>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button variant="brand" onClick={() => void submit()} disabled={!canSubmit}>
            {t('common.create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
