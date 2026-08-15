import { useEffect, useState } from 'react';
import { api, type TaskArtifacts, type TaskChanges } from '@/lib/api';

/**
 * useTaskArtifacts / useTaskChanges — I-2a tab-badge data lifting.
 *
 * `TaskArtifactsPanel` / `TaskChangesPanel` (the third/fourth-wave reusable
 * components) fetch lazily, only once their own tab is opened — exactly
 * right for the Inbox decision card, which shows one tab at a time and never
 * needs a count before the click. The task detail page's tab strip is
 * different: 「分頁帶計數 badge（產物 N、變更 N）」only works as a
 * decide-whether-to-click signal (design doc §3.2) if the count is already
 * known BEFORE the tab is opened. So `TaskBottomTabs` fetches once, eagerly,
 * here — on mount / `taskId` change — and feeds the result into the exact
 * same `TaskArtifactsList` / `TaskChangesList` pure-presentation components
 * the panels already export. No row-rendering logic is duplicated or
 * rewritten; only the fetch is lifted so the badge and the panel body share
 * one RPC instead of firing it twice.
 */
interface FetchState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

const IDLE: FetchState<never> = { data: null, loading: false, error: null };

export function useTaskArtifacts(taskId: string | undefined): FetchState<TaskArtifacts> {
  const [state, setState] = useState<FetchState<TaskArtifacts>>(IDLE);

  useEffect(() => {
    if (!taskId) {
      setState(IDLE);
      return;
    }
    let alive = true;
    setState({ data: null, loading: true, error: null });
    api.tasks
      .artifacts(taskId)
      .then((res) => {
        if (alive) setState({ data: res, loading: false, error: null });
      })
      .catch((e: unknown) => {
        if (alive) setState({ data: null, loading: false, error: e instanceof Error ? e.message : String(e) });
      });
    return () => {
      alive = false;
    };
  }, [taskId]);

  return state;
}

export function useTaskChanges(taskId: string | undefined): FetchState<TaskChanges> {
  const [state, setState] = useState<FetchState<TaskChanges>>(IDLE);

  useEffect(() => {
    if (!taskId) {
      setState(IDLE);
      return;
    }
    let alive = true;
    setState({ data: null, loading: true, error: null });
    api.tasks
      .changes(taskId)
      .then((res) => {
        if (alive) setState({ data: res, loading: false, error: null });
      })
      .catch((e: unknown) => {
        if (alive) setState({ data: null, loading: false, error: e instanceof Error ? e.message : String(e) });
      });
    return () => {
      alive = false;
    };
  }, [taskId]);

  return state;
}
