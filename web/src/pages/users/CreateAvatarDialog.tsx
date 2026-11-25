import { useId, useRef, useState, type FormEvent } from 'react';
import { useIntl } from 'react-intl';
import { api, type UserDetail } from '@/lib/api';
import { sanitizeErrorDetail } from '@/lib/error-message';
import { useAvailableModels } from '@/hooks/useAvailableModels';
import { ModelSelect } from '@/components/shared/ModelSelect';
import {
  Button, Input, Dialog, DialogContent, DialogHeader, DialogTitle,
  DialogDescription, DialogFooter,
} from '@/components/mds';
import { agentCreateErrorMessage, agentCreateNamesPresent } from '../agent-form/createHelpers';

export function CreateAvatarDialog({ user, onClose, onCreated }: {
  user: UserDetail;
  onClose: () => void;
  onCreated: () => void;
}) {
  const intl = useIntl();
  const fieldId = useId();
  // Keep the machine id independent of CJK display names and stable on retries.
  const [agentId] = useState(() => {
    // getRandomValues is also available on LAN HTTP dashboards.
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    return `avatar-${Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')}`;
  });
  const [name, setName] = useState(() => intl.formatMessage(
    { id: 'users.avatar.create.defaultName' }, { name: user.display_name },
  ));
  const [model, setModel] = useState('');
  const models = useAvailableModels();
  const [submitting, setSubmitting] = useState(false);
  const pending = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const canSubmit = agentCreateNamesPresent(agentId, name) && !!model.trim() && !submitting;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canSubmit || pending.current) return;
    pending.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await api.agents.create({
        name: agentId,
        display_name: name.trim(),
        avatar_for_user: user.id,
        role: 'specialist',
        model_preferred: model.trim(),
      });
      onCreated();
    } catch (cause) {
      // Prefer the gateway's zh-TW explanation, including structured errors.
      const zh = cause && typeof cause === 'object' && 'message_zh_TW' in cause
        && typeof cause.message_zh_TW === 'string' ? cause.message_zh_TW : null;
      const detail = sanitizeErrorDetail(zh ?? cause);
      setError(/[\u3040-\u30ff\u3400-\u9fff]/.test(detail)
        ? detail : agentCreateErrorMessage(cause, intl));
    } finally {
      pending.current = false;
      setSubmitting(false);
    }
  };

  return <Dialog open onOpenChange={open => !open && !pending.current && onClose()}>
    <DialogContent showClose={!submitting}>
      <DialogHeader>
        <DialogTitle>{intl.formatMessage({ id: 'users.avatar.create' })}</DialogTitle>
        <DialogDescription>{intl.formatMessage({ id: 'users.avatar.create.note' })}</DialogDescription>
      </DialogHeader>
      <form className="space-y-4" onSubmit={submit} aria-busy={submitting}>
        <div className="space-y-1.5">
          <label htmlFor={fieldId} className="text-sm font-medium">{intl.formatMessage({ id: 'agents.create.displayName' })}</label>
          <Input id={fieldId} value={name} onChange={event => setName(event.target.value)} required disabled={submitting} />
        </div>
        <div className="space-y-1.5">
          <label htmlFor={`${fieldId}-model`} className="text-sm font-medium">{intl.formatMessage({ id: 'agents.create.model' })}</label>
          <fieldset disabled={submitting}>
            <ModelSelect id={`${fieldId}-model`} value={model} onChange={setModel}
              models={models.models} loading={models.loading} error={models.error}
              discoveredAt={models.discoveredAt} refreshing={models.refreshing} onRefresh={models.refresh}
              ariaLabel={intl.formatMessage({ id: 'agents.create.model' })} />
          </fieldset>
        </div>
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={submitting} onClick={onClose}>{intl.formatMessage({ id: 'common.cancel' })}</Button>
          <Button type="submit" disabled={!canSubmit}>{intl.formatMessage({ id: submitting ? 'common.saving' : 'common.confirm' })}</Button>
        </DialogFooter>
      </form>
    </DialogContent>
  </Dialog>;
}
