import { useCallback, useEffect, useState } from 'react';
import { api } from '@/lib/api';

export interface AvailableModel {
  id: string;
  label: string;
  type: 'cloud' | 'local';
  provider?: string;
  /** Discovery source: live_api / cli_probe / help_parse / pty_probe / fallback. */
  source?: string;
  /** RFC3339 timestamp of the last probe for this provider. */
  fetched_at?: string;
  file?: string;
  size_bytes?: number;
}

export interface AvailableModelsResult {
  models: AvailableModel[];
  loading: boolean;
  /** Non-null when the model list could not be fetched — UI must degrade to
   *  manual input, never fabricate a list. */
  error: string | null;
  /** RFC3339 timestamp of the whole discovery run (null until first fetch). */
  discoveredAt: string | null;
  /** True while a manual `models.refresh` re-probe is in flight. */
  refreshing: boolean;
  /** Force a live re-probe of every provider, bypassing the local cache. */
  refresh: () => Promise<void>;
}

interface Snapshot {
  models: AvailableModel[];
  discoveredAt: string | null;
}

const TTL_MS = 60_000;
let cache: { at: number; snapshot: Snapshot } | null = null;
let inflight: Promise<Snapshot> | null = null;

function isFresh(): boolean {
  return cache !== null && Date.now() - cache.at < TTL_MS;
}

function fetchModels(): Promise<Snapshot> {
  if (isFresh()) return Promise.resolve(cache!.snapshot);
  if (inflight) return inflight;
  inflight = api.models
    .list()
    .then((res) => {
      const snapshot: Snapshot = {
        models: res?.models ?? [],
        discoveredAt: res?.discovered_at ?? null,
      };
      cache = { at: Date.now(), snapshot };
      return snapshot;
    })
    .finally(() => {
      inflight = null;
    });
  return inflight;
}

/** Force a live re-probe: invalidate the cache and call `models.refresh`. */
function refreshModels(): Promise<Snapshot> {
  cache = null;
  return api.models.refresh().then((res) => {
    const snapshot: Snapshot = {
      models: res?.models ?? [],
      discoveredAt: res?.discovered_at ?? null,
    };
    cache = { at: Date.now(), snapshot };
    return snapshot;
  });
}

/**
 * Fetches the live model registry (`models.list`) once on mount, deduped +
 * cached for 60s across all consumers. On failure returns an empty list plus an
 * `error` string so the UI can fall back to manual model entry. `refresh()`
 * triggers a server-side live re-probe (`models.refresh`).
 */
export function useAvailableModels(): AvailableModelsResult {
  const [models, setModels] = useState<AvailableModel[]>(() =>
    isFresh() ? cache!.snapshot.models : []
  );
  const [discoveredAt, setDiscoveredAt] = useState<string | null>(() =>
    isFresh() ? cache!.snapshot.discoveredAt : null
  );
  const [loading, setLoading] = useState(!isFresh());
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    fetchModels()
      .then((snap) => {
        if (!alive) return;
        setModels(snap.models);
        setDiscoveredAt(snap.discoveredAt);
        setError(null);
      })
      .catch((e: unknown) => {
        if (!alive) return;
        setModels([]);
        setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const snap = await refreshModels();
      setModels(snap.models);
      setDiscoveredAt(snap.discoveredAt);
      setError(null);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRefreshing(false);
    }
  }, []);

  return { models, loading, error, discoveredAt, refreshing, refresh };
}
