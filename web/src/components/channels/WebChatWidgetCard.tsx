import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Badge, Button, ErrorState, Input, SettingsCard, SettingsRow, SettingsSection } from '@/components/mds';
import { RowSwitch } from '@/pages/agent-form/form-rows';
import { tomlBool, tomlSecretSet } from '@/lib/config-toml';
import { useConfigForm, vBool } from '@/components/settings/useConfigForm';

export const WIDGET_KEY_MIN_CHARS = 16;

/** Random URL-safe key (24 bytes → 32 chars) for the embed snippet. */
export function generateWidgetKey(): string {
  const bytes = new Uint8Array(24);
  crypto.getRandomValues(bytes);
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/**
 * 通道 → WebChat: 網站聊天元件 (`config.toml [webchat] public_widget` +
 * `widget_key`), v1.68 W2. Re-read per request, so a save applies at once.
 * The key is write-only: the page only shows whether one is stored.
 */
export function WebChatWidgetCard() {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const [keySet, setKeySet] = useState(false);
  const [newKey, setNewKey] = useState('');
  const [keyError, setKeyError] = useState<string | null>(null);

  const form = useConfigForm((tables, res) => {
    // Prefer a structured flag when the gateway sends one; else the masked TOML.
    const structured = (res as Record<string, unknown> | undefined)?.webchat_widget_key_set;
    setKeySet(typeof structured === 'boolean' ? structured : tomlSecretSet(tables, 'webchat', 'widget_key'));
    return { 'webchat.public_widget': tomlBool(tables, 'webchat', 'public_widget', false) };
  });

  const enabled = vBool(form.values, 'webchat.public_widget');

  // v1.68: `widget_key: ""` removes the stored key. The gateway refuses it
  // while the widget is open, so turn the widget off first.
  const onClearKey = async () => {
    setKeyError(null);
    const res = await form.save({ webchat: { widget_key: '' } });
    if (res !== null) {
      setKeySet(false);
      setNewKey('');
    }
  };

  const onSave = async () => {
    const key = newKey.trim();
    if (key !== '' && key.length < WIDGET_KEY_MIN_CHARS) {
      setKeyError(intl.formatMessage({ id: 'channels.webchatWidget.keyTooShort' }, { min: WIDGET_KEY_MIN_CHARS }));
      return;
    }
    if (enabled && !keySet && key === '') {
      setKeyError(t('channels.webchatWidget.keyRequired'));
      return;
    }
    setKeyError(null);
    const res = await form.save(key !== '' ? { webchat: { widget_key: key } } : {});
    if (res !== null) {
      if (key !== '') setKeySet(true);
      setNewKey('');
    }
  };

  return (
    <SettingsSection
      title={t('channels.webchatWidget')}
      description={t('channels.webchatWidget.desc')}
    >
      {form.loadError != null && (
        <ErrorState
          variant="inline"
          error={form.loadError}
          title={t('errorState.manage.loadFailed')}
          onRetry={() => void form.load()}
        />
      )}
      <SettingsCard>
        <RowSwitch
          label={t('channels.webchatWidget.enabled')}
          description={t('channels.webchatWidget.enabled.help')}
          checked={enabled}
          onChange={(v) => form.set('webchat.public_widget', v)}
        />
        <SettingsRow
          label={
            <span className="flex items-center gap-2">
              {t('channels.webchatWidget.key')}
              <Badge variant={keySet ? 'secondary' : 'outline'} data-testid="widget-key-state">
                {keySet ? t('channels.webchatWidget.key.set') : t('channels.webchatWidget.key.unset')}
              </Badge>
            </span>
          }
          description={t('channels.webchatWidget.key.help')}
          tier="text"
        >
          <div className="flex gap-2">
            {/* Plain text on purpose: the operator copies a new key into the
                site's embed snippet; the stored key is never sent back. */}
            <Input
              type="text"
              value={newKey}
              spellCheck={false}
              autoComplete="off"
              placeholder={keySet ? '••••••••' : ''}
              aria-label={t('channels.webchatWidget.key')}
              onChange={(e) => {
                setNewKey(e.target.value);
                setKeyError(null);
              }}
            />
            <Button type="button" variant="secondary" size="sm" onClick={() => setNewKey(generateWidgetKey())}>
              {t('channels.webchatWidget.key.generate')}
            </Button>
          </div>
        </SettingsRow>
      </SettingsCard>
      {keyError && (
        <p role="alert" className="text-xs text-destructive">
          {keyError}
        </p>
      )}
      <div className="flex justify-end gap-2">
        {keySet && (
          <Button
            variant="outline"
            size="sm"
            onClick={() => void onClearKey()}
            disabled={form.saving || enabled}
            title={enabled ? t('channels.webchatWidget.key.clearBlocked') : undefined}
          >
            {t('channels.webchatWidget.key.clear')}
          </Button>
        )}
        <Button variant="brand" size="sm" onClick={() => void onSave()} disabled={form.saving}>
          {form.saving ? t('common.saving') : t('common.save')}
        </Button>
      </div>
    </SettingsSection>
  );
}
