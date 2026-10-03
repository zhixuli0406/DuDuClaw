import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import { toast, formatError } from '@/lib/toast';
import {
  changedKeys,
  changedPayload,
  normalizeRestartRequired,
  parseTomlSubset,
  type FlatValues,
  type TomlTables,
} from '@/lib/config-toml';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

export type SystemConfigResponse = Awaited<ReturnType<typeof api.system.config>>;

/**
 * Diff-based `config.toml` form for the v1.68 settings cards (W2).
 *
 * Values are a flat `{ "<toml.path>": value }` map. `save()` sends only the
 * leaves that differ from what was loaded (plan principle 3), merged with any
 * `extra` write-only fields (secrets typed by the operator). A save that
 * changes nothing does not call the gateway — `system.update_config` answers
 * "No valid fields" for an empty payload. `restart_required` keys feed the
 * persistent restart banner. `save()` resolves to the restart keys of a
 * successful save (possibly empty) or `null` when nothing was sent / it failed.
 */
export function useConfigForm(read: (tables: TomlTables, res: SystemConfigResponse) => FlatValues) {
  const intl = useIntl();
  const readRef = useRef(read);
  readRef.current = read;
  const [values, setValues] = useState<FlatValues>({});
  const initialRef = useRef<FlatValues>({});
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<unknown>(null);
  const [lastRestart, setLastRestart] = useState<string[]>([]);
  const addRestart = useRestartRequiredStore((s) => s.add);

  const load = useCallback(() => {
    setLoadError(null);
    return api.system
      .config()
      .then((res) => {
        const raw = typeof res?.config === 'string' ? res.config : '';
        const next = readRef.current(parseTomlSubset(raw), res);
        initialRef.current = next;
        setValues(next);
        setLoaded(true);
      })
      .catch((e) => {
        setLoadError(e);
        toast.error(intl.formatMessage({ id: 'toast.error.loadFailed' }, { message: formatError(e) }));
      });
  }, [intl]);

  useEffect(() => {
    void load();
  }, [load]);

  const set = useCallback((key: string, value: unknown) => {
    setValues((prev) => ({ ...prev, [key]: value }));
  }, []);

  const dirtyKeys = changedKeys(initialRef.current, values);

  const save = useCallback(
    async (extra: Record<string, unknown> = {}): Promise<string[] | null> => {
      const payload = { ...changedPayload(initialRef.current, values) };
      for (const [k, v] of Object.entries(extra)) {
        const existing = payload[k];
        payload[k] =
          existing && typeof existing === 'object' && v && typeof v === 'object' && !Array.isArray(v)
            ? { ...(existing as Record<string, unknown>), ...(v as Record<string, unknown>) }
            : v;
      }
      if (Object.keys(payload).length === 0) {
        toast.info(intl.formatMessage({ id: 'settings.noChanges' }));
        return null;
      }
      setSaving(true);
      setSaveError(null);
      try {
        const res = await api.system.updateConfig(payload);
        const restart = normalizeRestartRequired(res?.restart_required, '');
        setLastRestart(restart);
        if (restart.length > 0) addRestart(restart);
        initialRef.current = values;
        toast.success(intl.formatMessage({ id: 'settings.general.saved' }));
        return restart;
      } catch (e) {
        setSaveError(e);
        toast.error(intl.formatMessage({ id: 'toast.error.saveFailed' }, { message: formatError(e) }));
        return null;
      } finally {
        setSaving(false);
      }
    },
    [values, intl, addRestart],
  );

  return { values, set, load, save, loaded, loadError, saving, saveError, dirtyKeys, lastRestart };
}

/** Typed getters for the flat value map (avoids casts at every call site). */
export function vBool(values: FlatValues, key: string): boolean {
  return values[key] === true;
}
export function vNum(values: FlatValues, key: string, fallback = 0): number {
  const v = values[key];
  return typeof v === 'number' && Number.isFinite(v) ? v : fallback;
}
export function vStr(values: FlatValues, key: string): string {
  const v = values[key];
  return typeof v === 'string' ? v : '';
}
export function vList(values: FlatValues, key: string): string[] {
  const v = values[key];
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
}
