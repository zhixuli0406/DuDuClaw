import { useState } from 'react';
import { useIntl } from 'react-intl';
import { Plus, Trash2 } from 'lucide-react';
import type { AgentKvType, BuiltinToolEntry } from '@/lib/api';
import { Button, Input } from '@/components/mds';
import { ChipEditor } from '@/components/shared/ChipEditor';
import { ToolCatalogPicker } from '@/components/shared/ToolCatalogPicker';
import { selectClass } from '@/components/shared/controlClass';
import { cn } from '@/lib/utils';
import { type KvRow, KV_SECTION_SUGGESTIONS } from './defaults';
import { parseKvRow } from './editPayload';
import { FieldBlock } from './form-rows';

// v1.68 W1 — controls added to the AI-employee edit page.

/** Bare MCP tool name, as matched exactly by the approval / irreversibility /
 *  scoped-grant gates. */
export const TOOL_NAME_RE = /^[a-z0-9_]+$/;

const offerBareMcpTools = (e: BuiltinToolEntry) => e.scope !== '' && TOOL_NAME_RE.test(e.name);

/**
 * Tag-style list of tool names with the catalogue picker (writes the bare
 * `name`, not `mcp__duduclaw__…`, because these gates compare bare names) and
 * free text validated against `^[a-z0-9_]+$`. An invalid entry is refused
 * with an inline message instead of being saved.
 */
export function ToolNameListField({
  label,
  description,
  values,
  onChange,
  disabled,
}: {
  label: string;
  description?: string;
  values: ReadonlyArray<string>;
  onChange: (next: string[]) => void;
  disabled?: boolean;
}) {
  const intl = useIntl();
  const [invalid, setInvalid] = useState<string | null>(null);
  const accept = (next: string[]) => {
    const bad = next.find((v) => !TOOL_NAME_RE.test(v));
    if (bad !== undefined) {
      setInvalid(bad);
      return;
    }
    setInvalid(null);
    onChange(next);
  };
  return (
    <FieldBlock label={label} description={description}>
      <div className="space-y-2">
        <ChipEditor
          values={values}
          onChange={accept}
          placeholder="send_message"
          addLabel={intl.formatMessage({ id: 'common.add' })}
        />
        {!disabled && (
          <ToolCatalogPicker
            triggerLabel={intl.formatMessage({ id: 'agents.cap.toolPicker.add' })}
            selected={values}
            onChange={accept}
            valueKey="name"
            filter={offerBareMcpTools}
          />
        )}
        {invalid !== null && (
          <p role="alert" className="text-xs text-destructive">
            {intl.formatMessage({ id: 'agents.v168.toolName.invalid' }, { name: invalid })}
          </p>
        )}
      </div>
    </FieldBlock>
  );
}

const KV_TYPES: ReadonlyArray<AgentKvType> = ['string', 'integer', 'float', 'boolean', 'string_array'];

/**
 * Advanced typed key/value editor. Each row is `[section] key = value` with an
 * explicit type, so `cli_bare_mode = true` is written as a TOML boolean rather
 * than the string `"true"` (audit F3: a mistyped value made the employee fail
 * to load). The server re-validates the whole file and rejects the save with
 * its parse error, which the page shows in `serverError`.
 */
export function TypedKvTable({
  rows,
  onChange,
  serverError,
}: {
  rows: ReadonlyArray<KvRow>;
  onChange: (next: KvRow[]) => void;
  serverError?: string | null;
}) {
  const intl = useIntl();
  const t = (id: string) => intl.formatMessage({ id });
  const update = (idx: number, patch: Partial<KvRow>) =>
    onChange(rows.map((r, i) => (i === idx ? { ...r, ...patch } : r)));
  const remove = (idx: number) => onChange(rows.filter((_, i) => i !== idx));
  const add = () => onChange([...rows, { section: 'prompt', key: '', value: '', type: 'string' }]);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <h4 className="text-xs font-semibold uppercase text-muted-foreground">{t('agents.v168.kv.title')}</h4>
        <Button type="button" size="sm" variant="ghost" onClick={add}>
          <Plus />
          {t('common.add')}
        </Button>
      </div>
      <datalist id="agent-kv-sections">
        {KV_SECTION_SUGGESTIONS.map((s) => (
          <option key={s} value={s} />
        ))}
      </datalist>
      {rows.length === 0 ? (
        <p className="py-1 text-center text-xs text-muted-foreground">{t('agents.adv.kv.empty')}</p>
      ) : (
        <div className="space-y-2">
          {rows.map((r, idx) => {
            const parsed = parseKvRow(r);
            const err = parsed.kind === 'error' ? parsed.error : null;
            return (
              <div key={idx} className="space-y-1">
                <div className="flex flex-wrap items-center gap-2 sm:flex-nowrap">
                  <Input
                    list="agent-kv-sections"
                    value={r.section}
                    onChange={(e) => update(idx, { section: e.target.value })}
                    placeholder="prompt"
                    aria-label={t('agents.v168.kv.section')}
                    aria-invalid={err === 'section'}
                    className="w-28 shrink-0"
                  />
                  <Input
                    value={r.key}
                    onChange={(e) => update(idx, { key: e.target.value })}
                    placeholder="key"
                    aria-label={t('agents.v168.kv.key')}
                    aria-invalid={err === 'key'}
                    className="min-w-0 flex-1"
                  />
                  <select
                    value={r.type}
                    onChange={(e) => update(idx, { type: e.target.value as AgentKvType, value: e.target.value === 'boolean' ? 'true' : r.value })}
                    aria-label={t('agents.v168.kv.type')}
                    className={cn(selectClass, 'w-32 shrink-0')}
                  >
                    {KV_TYPES.map((ty) => (
                      <option key={ty} value={ty}>
                        {t(`agents.v168.kv.type.${ty}`)}
                      </option>
                    ))}
                  </select>
                  {r.type === 'boolean' ? (
                    <select
                      value={r.value === 'false' ? 'false' : 'true'}
                      onChange={(e) => update(idx, { value: e.target.value })}
                      aria-label={t('agents.v168.kv.value')}
                      className={cn(selectClass, 'min-w-0 flex-1')}
                    >
                      <option value="true">true</option>
                      <option value="false">false</option>
                    </select>
                  ) : (
                    <Input
                      value={r.value}
                      onChange={(e) => update(idx, { value: e.target.value })}
                      placeholder={r.type === 'string_array' ? 'a, b, c' : 'value'}
                      aria-label={t('agents.v168.kv.value')}
                      aria-invalid={err === 'value'}
                      className="min-w-0 flex-1"
                    />
                  )}
                  <Button
                    type="button"
                    size="icon-sm"
                    variant="ghost"
                    onClick={() => remove(idx)}
                    className="shrink-0 text-destructive hover:bg-destructive/10 hover:text-destructive"
                    aria-label={t('agents.v168.kv.remove')}
                  >
                    <Trash2 />
                  </Button>
                </div>
                {err && <p className="px-1 text-xs text-destructive">{t(`agents.v168.kv.error.${err}`)}</p>}
              </div>
            );
          })}
        </div>
      )}
      <p className="text-xs text-muted-foreground">{t('agents.v168.kv.removeNote')}</p>
      {serverError && (
        <p role="alert" className="rounded-md bg-destructive/10 px-3 py-2 font-mono text-xs text-destructive">
          {intl.formatMessage({ id: 'agents.v168.kv.serverError' }, { message: serverError })}
        </p>
      )}
    </div>
  );
}
