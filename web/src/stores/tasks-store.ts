import { create } from 'zustand';
import {
  api,
  type TaskInfo,
  type TaskStatus,
  type TaskPriority,
  type TaskCreateParams,
  type TaskUpdateParams,
  type ActivityEvent,
  type TaskComment,
} from '@/lib/api';
import { client } from '@/lib/ws-client';

interface TasksStore {
  readonly tasks: ReadonlyArray<TaskInfo>;
  readonly activities: ReadonlyArray<ActivityEvent>;
  /** Comments keyed by task id (oldest first). Loaded on demand per task. */
  readonly comments: Readonly<Record<string, ReadonlyArray<TaskComment>>>;
  readonly loading: boolean;
  /**
   * The raw thrown value. Pages render it through `ErrorState` /
   * `useErrorMessage`, which classify it into plain language and keep the
   * technical string behind a disclosure — so it is deliberately NOT
   * pre-flattened to `String(e)` here.
   */
  readonly error: unknown;
  /** L2: the last task list came back partial (some bound agents failed) —
   *  rows that arrived are kept; cleared by the next fully successful load. */
  readonly tasksPartialError: unknown;
  /** Same signal for the activity feed. */
  readonly activitiesPartialError: unknown;
  clearError: () => void;
  readonly filterAgent: string | null;
  readonly filterPriority: TaskPriority | null;
  fetchTasks: (filters?: { status?: TaskStatus; agent_id?: string }) => Promise<void>;
  createTask: (params: TaskCreateParams) => Promise<TaskInfo | null>;
  updateTask: (taskId: string, fields: TaskUpdateParams) => Promise<void>;
  removeTask: (taskId: string) => Promise<void>;
  moveTask: (taskId: string, newStatus: TaskStatus) => Promise<void>;
  assignTask: (taskId: string, agentId: string) => Promise<void>;
  setFilterAgent: (agentId: string | null) => void;
  setFilterPriority: (priority: TaskPriority | null) => void;
  fetchActivities: (params?: { agent_id?: string; limit?: number }) => Promise<void>;
  fetchComments: (taskId: string) => Promise<void>;
  addComment: (taskId: string, body: string) => Promise<TaskComment | null>;
}

/** Upsert a task by id (immutable). If a task with the same id already exists
 *  it is replaced in place; otherwise the incoming task is appended. This gives
 *  the optimistic `createTask` add and the `task.created` WebSocket echo the
 *  same identity — so a freshly created task never renders as two cards (#2).
 *  Pure — exported for the store reducer and its tests. */
export function upsertTask(
  existing: ReadonlyArray<TaskInfo>,
  incoming: TaskInfo,
): ReadonlyArray<TaskInfo> {
  let found = false;
  const next = existing.map((t) => {
    if (t.id === incoming.id) {
      found = true;
      return incoming;
    }
    return t;
  });
  return found ? next : [...next, incoming];
}

/** Insert a comment into a task's list, de-duplicating by id and keeping the
 *  list oldest-first. Pure — exported for the store reducer and its tests. */
export function mergeComment(
  existing: ReadonlyArray<TaskComment>,
  incoming: TaskComment,
): ReadonlyArray<TaskComment> {
  if (existing.some((c) => c.id === incoming.id)) return existing;
  return [...existing, incoming].sort((a, b) => a.created_at.localeCompare(b.created_at));
}

export const useTasksStore = create<TasksStore>((set, get) => {
  // Latest-request-wins: fan-out made un-scoped lists slower, so an older
  // response must never overwrite a newer one (rows and partial flag alike).
  let tasksSeq = 0;
  let activitiesSeq = 0;
  // Subscribe to real-time task updates
  client.subscribe('task.updated', (payload) => {
    const data = payload as TaskInfo;
    set({
      tasks: get().tasks.map((t) => (t.id === data.id ? data : t)),
    });
  });

  client.subscribe('task.created', (payload) => {
    const data = payload as TaskInfo;
    // Upsert, not append: the creating client already added this task
    // optimistically in `createTask`, so a blind append would duplicate the
    // card until the next refetch (#2).
    set({ tasks: upsertTask(get().tasks, data) });
  });

  client.subscribe('task.removed', (payload) => {
    const data = payload as { task_id: string };
    set({ tasks: get().tasks.filter((t) => t.id !== data.task_id) });
  });

  // Subscribe to activity events
  client.subscribe('activity.new', (payload) => {
    const event = payload as ActivityEvent;
    set({ activities: [event, ...get().activities].slice(0, 100) });
  });

  // L2: real-time task comments — merge into the per-task list (dedup by id so
  // the author's own optimistic insert isn't duplicated by the broadcast echo).
  client.subscribe('task.comment', (payload) => {
    const c = payload as TaskComment;
    const existing = get().comments[c.task_id] ?? [];
    set({ comments: { ...get().comments, [c.task_id]: mergeComment(existing, c) } });
  });

  return {
    tasks: [],
    activities: [],
    comments: {},
    loading: false,
    error: null,
    tasksPartialError: null,
    activitiesPartialError: null,
    filterAgent: null,
    filterPriority: null,

    fetchTasks: async (filters) => {
      const seq = ++tasksSeq;
      set({ loading: true, error: null });
      try {
        const result = await api.tasks.list(filters);
        if (seq !== tasksSeq) return; // a newer fetch owns the state now
        set({ tasks: result?.tasks ?? [], loading: false, tasksPartialError: result?.partial_error ?? null });
      } catch (e) {
        if (seq !== tasksSeq) return;
        // A total failure is not "partial": drop any stale partial flag.
        set({ error: e, loading: false, tasksPartialError: null });
      }
    },

    createTask: async (params) => {
      try {
        const result = await api.tasks.create(params);
        const task = result.task;
        // Upsert so a racing `task.created` WebSocket echo can't double-add it.
        set({ tasks: upsertTask(get().tasks, task) });
        return task;
      } catch (e) {
        set({ error: e });
        return null;
      }
    },

    updateTask: async (taskId, fields) => {
      try {
        const result = await api.tasks.update(taskId, fields);
        set({
          tasks: get().tasks.map((t) => (t.id === taskId ? result.task : t)),
        });
      } catch (e) {
        set({ error: e });
      }
    },

    removeTask: async (taskId) => {
      try {
        await api.tasks.remove(taskId);
        set({ tasks: get().tasks.filter((t) => t.id !== taskId) });
      } catch (e) {
        set({ error: e });
      }
    },

    moveTask: async (taskId, newStatus) => {
      // Optimistic update
      const prev = get().tasks;
      set({
        tasks: prev.map((t) =>
          t.id === taskId ? { ...t, status: newStatus, updated_at: new Date().toISOString() } : t
        ),
      });
      try {
        await api.tasks.update(taskId, { status: newStatus });
      } catch (e) {
        // Rollback on failure
        set({ tasks: prev, error: e });
      }
    },

    assignTask: async (taskId, agentId) => {
      try {
        const result = await api.tasks.assign(taskId, agentId);
        set({
          tasks: get().tasks.map((t) => (t.id === taskId ? result.task : t)),
        });
      } catch (e) {
        set({ error: e });
      }
    },

    clearError: () => set({ error: null }),

    setFilterAgent: (agentId) => set({ filterAgent: agentId }),
    setFilterPriority: (priority) => set({ filterPriority: priority }),

    fetchActivities: async (params) => {
      const seq = ++activitiesSeq;
      try {
        const result = await api.activity.list(params);
        if (seq !== activitiesSeq) return;
        set({ activities: result?.events ?? [], activitiesPartialError: result?.partial_error ?? null });
      } catch (e) {
        if (seq !== activitiesSeq) return;
        set({ error: e, activitiesPartialError: null });
      }
    },

    fetchComments: async (taskId) => {
      try {
        const result = await api.tasks.comments(taskId);
        set({ comments: { ...get().comments, [taskId]: result?.comments ?? [] } });
      } catch (e) {
        set({ error: e });
      }
    },

    addComment: async (taskId, body) => {
      try {
        const result = await api.tasks.comment(taskId, body);
        const c = result.comment;
        const existing = get().comments[taskId] ?? [];
        set({ comments: { ...get().comments, [taskId]: mergeComment(existing, c) } });
        return c;
      } catch (e) {
        set({ error: e });
        return null;
      }
    },
  };
});
