import { useState } from 'react';
import { useIntl } from 'react-intl';
import {
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/mds';
import { selectClass } from '@/components/shared/controlClass';
import { toast, formatError } from '@/lib/toast';
import {
  KB_SENSITIVITIES,
  knowledgeApi,
  type KbAgentDataSources,
  type KbCollection,
  type KbSensitivity,
} from '@/lib/knowledge-api';

/**
 * First upload for an employee: its own collection is created here.
 * The uploader picks the sensitivity (default 公司內部) and whether the
 * people already working with this employee may read the files too.
 */
export function CreateOwnDialog({
  agentId,
  agentName,
  defaults,
  onClose,
  onReady,
}: {
  agentId: string;
  agentName: string;
  defaults: KbAgentDataSources['default_readers'];
  onClose: () => void;
  onReady: (c: KbCollection, bindingChanged: boolean) => void;
}) {
  const intl = useIntl();
  const t = (id: string, v?: Record<string, string | number>) => intl.formatMessage({ id }, v);
  const [sensitivity, setSensitivity] = useState<KbSensitivity>('internal');
  const [share, setShare] = useState(true);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    setBusy(true);
    try {
      const r = await knowledgeApi.ensureOwnCollection(agentId, { sensitivity, shareChatUsers: share });
      onReady(r.collection, r.binding_changed);
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setBusy(false);
    }
  };

  const names = defaults.map((d) => d.label);

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{t('agentSources.create.title')}</DialogTitle>
          <DialogDescription>{t('agentSources.create.desc', { name: agentName })}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="space-y-1.5">
            <label htmlFor="agent-own-sensitivity" className="text-sm font-medium">
              {t('kb.field.sensitivity')}
            </label>
            <select
              id="agent-own-sensitivity"
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
          <div className="space-y-2 rounded-lg border border-surface-border p-3">
            <p className="text-sm font-medium">{t('agentSources.create.whoTitle')}</p>
            <p className="text-sm text-muted-foreground">{t('agentSources.create.whoAlways')}</p>
            {names.length > 0 ? (
              <label className="flex items-start gap-2 text-sm">
                <Checkbox checked={share} onCheckedChange={(v) => setShare(v === true)} aria-label={t('agentSources.create.share')} />
                <span>
                  {t('agentSources.create.share')}
                  <span className="block text-xs text-muted-foreground">
                    {intl.formatList(names, { type: 'conjunction' })}
                  </span>
                </span>
              </label>
            ) : (
              <p className="text-xs text-muted-foreground">{t('agentSources.create.nobodyElse')}</p>
            )}
            <p className="text-xs text-muted-foreground">{t('agentSources.create.later')}</p>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button variant="brand" onClick={() => void submit()} disabled={busy}>
            {t('agentSources.create.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
