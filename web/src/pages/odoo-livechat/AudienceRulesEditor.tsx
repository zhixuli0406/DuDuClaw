import { useEffect, useState } from 'react';
import { useIntl } from 'react-intl';
import { ArrowDown, ArrowUp, Plus, Trash2 } from 'lucide-react';
import type { OdooLivechatRule, OdooLivechatTestResult } from '@/lib/api';
import { Badge, Button, Checkbox, Input, SettingsCard, SettingsRow } from '@/components/mds';
import { RowSelect, RowSwitch, RowText } from '@/pages/agent-form/form-rows';
import type { SelectOption } from '@/components/settings/controls';
import { emptyRule, isCatchAll, joinList, moveItem, parseIds, parseList, toggleId } from './livechat-form';

type Pickers = Pick<OdooLivechatTestResult, 'channels' | 'chatbot_scripts'> | null;

/** Text input for a list value; parses on blur so commas can be typed. */
export function ListInput({
  values,
  onChange,
  placeholder,
  ids,
}: {
  values: readonly (string | number)[];
  onChange: (v: string[]) => void;
  placeholder?: string;
  ids?: boolean;
}) {
  const [text, setText] = useState(joinList(values));
  useEffect(() => setText(joinList(values)), [values]);
  return (
    <Input
      value={text}
      placeholder={placeholder}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => onChange(ids ? parseIds(text).map(String) : parseList(text))}
    />
  );
}

/** Checkbox list when the test listed the options, id text otherwise. */
function IdPicker({
  label,
  description,
  selected,
  options,
  onChange,
}: {
  label: string;
  description?: string;
  selected: number[];
  options: Array<{ id: number; name: string }> | undefined;
  onChange: (ids: number[]) => void;
}) {
  const intl = useIntl();
  if (!options || options.length === 0) {
    return (
      <SettingsRow label={label} description={description} tier="text">
        <ListInput
          ids
          values={selected}
          onChange={(v) => onChange(v.map(Number))}
          placeholder={intl.formatMessage({ id: 'odoo.livechat.idsPlaceholder' })}
        />
      </SettingsRow>
    );
  }
  return (
    <SettingsRow label={label} description={description} tier="text">
      <div className="flex flex-wrap justify-end gap-x-4 gap-y-2">
        {options.map((o) => (
          <label key={o.id} className="inline-flex items-center gap-2 text-sm">
            <Checkbox
              checked={selected.includes(o.id)}
              onCheckedChange={(v) => onChange(toggleId(selected, o.id, Boolean(v)))}
            />
            {o.name}
          </label>
        ))}
      </div>
    </SettingsRow>
  );
}

export function AudienceRulesEditor({
  rules,
  onChange,
  agentOptions,
  pickers,
}: {
  rules: OdooLivechatRule[];
  onChange: (rules: OdooLivechatRule[]) => void;
  agentOptions: SelectOption[];
  pickers: Pickers;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });

  const update = (i: number, patch: Partial<OdooLivechatRule>) =>
    onChange(rules.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  const visitorOptions: SelectOption[] = [
    { value: 'any', label: t('odoo.livechat.rule.visitor.any') },
    { value: 'logged_in', label: t('odoo.livechat.rule.visitor.loggedIn') },
    { value: 'anonymous', label: t('odoo.livechat.rule.visitor.anonymous') },
  ];

  return (
    <div className="space-y-4">
      {rules.length === 0 && (
        <p className="rounded-lg border border-dashed border-surface-border px-4 py-6 text-center text-sm text-muted-foreground">
          {t('odoo.livechat.rules.empty')}
        </p>
      )}
      {rules.map((rule, i) => (
        <SettingsCard key={i}>
          <div className="flex flex-wrap items-center justify-between gap-2 pb-2">
            <div className="flex items-center gap-2">
              <Badge variant="secondary">#{i + 1}</Badge>
              <span className="text-sm font-medium">{rule.name || t('odoo.livechat.rule.untitled')}</span>
              {isCatchAll(rule) && <Badge variant="outline">{t('odoo.livechat.rule.catchAll')}</Badge>}
            </div>
            <div className="flex items-center gap-1">
              <Button
                variant="ghost"
                size="icon"
                aria-label={t('odoo.livechat.rule.moveUp')}
                disabled={i === 0}
                onClick={() => onChange(moveItem(rules, i, i - 1))}
              >
                <ArrowUp />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                aria-label={t('odoo.livechat.rule.moveDown')}
                disabled={i === rules.length - 1}
                onClick={() => onChange(moveItem(rules, i, i + 1))}
              >
                <ArrowDown />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                aria-label={t('odoo.livechat.rule.delete')}
                onClick={() => onChange(rules.filter((_, j) => j !== i))}
              >
                <Trash2 />
              </Button>
            </div>
          </div>
          <RowText label={t('odoo.livechat.rule.name')} value={rule.name} onChange={(v) => update(i, { name: v })} />
          <RowSelect
            label={t('odoo.livechat.rule.agent')}
            value={rule.agent}
            onChange={(v) => update(i, { agent: v })}
            options={agentOptions}
          />
          <RowSwitch label={t('odoo.livechat.rule.enabled')} checked={rule.enabled} onChange={(v) => update(i, { enabled: v })} />
          <IdPicker
            label={t('odoo.livechat.rule.channels')}
            selected={rule.livechat_channel_ids}
            options={pickers?.channels}
            onChange={(ids) => update(i, { livechat_channel_ids: ids })}
          />
          <IdPicker
            label={t('odoo.livechat.rule.chatbots')}
            description={t('odoo.livechat.rule.chatbotsDesc')}
            selected={rule.chatbot_script_ids}
            options={pickers?.chatbot_scripts}
            onChange={(ids) => update(i, { chatbot_script_ids: ids })}
          />
          <RowSelect
            label={t('odoo.livechat.rule.visitor')}
            value={rule.visitor}
            onChange={(v) => update(i, { visitor: v as OdooLivechatRule['visitor'] })}
            options={visitorOptions}
          />
          <SettingsRow label={t('odoo.livechat.rule.tags')} description={t('odoo.livechat.rule.tagsDesc')} tier="text">
            <ListInput values={rule.partner_tags} onChange={(v) => update(i, { partner_tags: v })} placeholder="VIP" />
          </SettingsRow>
          <SettingsRow label={t('odoo.livechat.rule.countries')} description={t('odoo.livechat.rule.countriesDesc')} tier="text">
            <ListInput values={rule.country_codes} onChange={(v) => update(i, { country_codes: v })} placeholder="TW, HK" />
          </SettingsRow>
          <SettingsRow label={t('odoo.livechat.rule.langs')} description={t('odoo.livechat.rule.langsDesc')} tier="text">
            <ListInput values={rule.langs} onChange={(v) => update(i, { langs: v })} placeholder="zh_TW, en" />
          </SettingsRow>
          <SettingsRow label={t('odoo.livechat.rule.keywords')} description={t('odoo.livechat.rule.keywordsDesc')} tier="text">
            <ListInput values={rule.keywords} onChange={(v) => update(i, { keywords: v })} placeholder="退貨, 換貨" />
          </SettingsRow>
        </SettingsCard>
      ))}
      <Button variant="outline" size="sm" onClick={() => onChange([...rules, emptyRule()])}>
        <Plus />
        {t('odoo.livechat.rules.add')}
      </Button>
    </div>
  );
}
