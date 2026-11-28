import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from './api';

/** Admin-only name lookup. Unknown members never fall back to their ids. */
export function useUserNames(): {
  nameOf: (id: string | null | undefined) => string;
  loading: boolean;
  failed: boolean;
} {
  const intl = useIntl();
  const request = useRef<ReturnType<typeof api.users.list> | null>(null);
  const [names, setNames] = useState<Map<string, string>>(new Map());
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let active = true;
    // Reuse the request across StrictMode effect replay, but never across mounts.
    request.current ??= api.users.list();
    void request.current.then(({ users }) => {
      if (active) {
        setNames(new Map(users.map((user) => [user.id, user.display_name.trim() || user.email])));
        setLoading(false);
      }
    }).catch(() => {
      if (active) { setFailed(true); setLoading(false); }
    });
    return () => { active = false; };
  }, []);

  const nameOf = useCallback((id: string | null | undefined): string => {
    if (id == null || loading) return '';
    if (failed) return intl.formatMessage({ id: 'userNames.failed' });
    return names.get(id) ?? intl.formatMessage({ id: 'userNames.removed' });
  }, [failed, intl, loading, names]);
  return { nameOf, loading, failed };
}
