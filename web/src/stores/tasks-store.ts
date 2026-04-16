import { create } from 'zustand';
import {
  api,
  type TaskInfo,
  type TaskStatus,
  type TaskPriority,
  type TaskCreateParams,
  type TaskUpdateParams,
  type ActivityEvent,
} from '@/lib/api';
import { client } from '@/lib/ws-client';

interface TasksStore {
  readonly tasks: ReadonlyArray<TaskInfo>;
  readonly activities: ReadonlyArray<ActivityEvent>;
  readonly loading: boolean;
  readonly error: string | null;
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
}

export const useTasksStore = create<TasksStore>((set, get) => {
  // Subscribe to real-time task updates
  client.subscribe('task.updated', (payload) => {
    const data = payload as TaskInfo;
    set({
      tasks: get().tasks.map((t) => (t.id === data.id ? data : t)),
    });
  });

  client.subscribe('task.created', (payload) => {
    const data = payload as TaskInfo;
    set({ tasks: [...get().tasks, data] });
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

  return {
    tasks: [],
    activities: [],
    loading: false,
    error: null,
    filterAgent: null,
    filterPriority: null,

    fetchTasks: async (filters) => {
      set({ loading: true, error: null });
      try {
        const result = await api.tasks.list(filters);
        set({ tasks: result?.tasks ?? [], loading: false });
      } catch (e) {
        set({ error: String(e), loading: false });
      }
    },

    createTask: async (params) => {
      try {
        const result = await api.tasks.create(params);
        const task = result.task;
        set({ tasks: [...get().tasks, task] });
        return task;
      } catch (e) {
        set({ error: String(e) });
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
        set({ error: String(e) });
      }
    },

    removeTask: async (taskId) => {
      try {
        await api.tasks.remove(taskId);
        set({ tasks: get().tasks.filter((t) => t.id !== taskId) });
      } catch (e) {
        set({ error: String(e) });
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
        set({ tasks: prev, error: String(e) });
      }
    },

    assignTask: async (taskId, agentId) => {
      try {
        const result = await api.tasks.assign(taskId, agentId);
        set({
          tasks: get().tasks.map((t) => (t.id === taskId ? result.task : t)),
        });
      } catch (e) {
        set({ error: String(e) });
      }
    },

    setFilterAgent: (agentId) => set({ filterAgent: agentId }),
    setFilterPriority: (priority) => set({ filterPriority: priority }),

    fetchActivities: async (params) => {
      try {
        const result = await api.activity.list(params);
        set({ activities: result?.events ?? [] });
      } catch (e) {
        set({ error: String(e) });
      }
    },
  };
});
