import { create } from 'zustand';
import {
  api,
  type PlanInfo,
  type PlanStep,
  type PlanCreateParams,
  type PlanStatus,
  type PlanStepUpdateParams,
} from '@/lib/api';
import { client } from '@/lib/ws-client';

/**
 * U4 co-edited plans store. The plan is shared: the user edits it here, the AI
 * employee edits it through the `plan_get` / `plan_update_step` MCP tools.
 * Server order is canonical — every mutation refetches the affected plan so
 * the integer-gap ordering the gateway computed is what renders (no client
 * guess), and a `plan.updated` broadcast (co-edit signal) triggers the same
 * refetch for edits made by the agent or another tab.
 */
interface PlansStore {
  readonly plans: ReadonlyArray<PlanInfo>;
  /** Steps keyed by plan id, in display order. */
  readonly steps: Readonly<Record<string, ReadonlyArray<PlanStep>>>;
  readonly loading: boolean;
  readonly error: string | null;
  fetchPlans: (filters?: { agent_id?: string; status?: PlanStatus }) => Promise<void>;
  fetchPlan: (planId: string) => Promise<void>;
  createPlan: (params: PlanCreateParams) => Promise<PlanInfo | null>;
  updatePlan: (
    planId: string,
    fields: { title?: string; description?: string; status?: PlanStatus },
  ) => Promise<void>;
  removePlan: (planId: string) => Promise<void>;
  addStep: (
    planId: string,
    params: { text: string; assignee_kind?: PlanStep['assignee_kind']; assignee?: string; position?: number },
  ) => Promise<void>;
  updateStep: (planId: string, stepId: string, fields: PlanStepUpdateParams) => Promise<void>;
  removeStep: (planId: string, stepId: string) => Promise<void>;
}

export const usePlansStore = create<PlansStore>((set, get) => {
  // Co-edit signal: an agent tick / another tab's edit refreshes the panel.
  client.subscribe('plan.updated', (payload) => {
    const data = payload as { plan_id?: string };
    void get().fetchPlans();
    if (data.plan_id && get().steps[data.plan_id]) void get().fetchPlan(data.plan_id);
  });

  const refreshAfterMutation = async (planId: string) => {
    await Promise.all([get().fetchPlan(planId), get().fetchPlans()]);
  };

  return {
    plans: [],
    steps: {},
    loading: false,
    error: null,

    fetchPlans: async (filters) => {
      set({ loading: true });
      try {
        const { plans } = await api.plans.list(filters);
        set({ plans, loading: false, error: null });
      } catch (e) {
        set({ loading: false, error: e instanceof Error ? e.message : String(e) });
      }
    },

    fetchPlan: async (planId) => {
      try {
        const { plan, steps } = await api.plans.get(planId);
        set({
          steps: { ...get().steps, [planId]: steps },
          plans: get().plans.some((p) => p.id === plan.id)
            ? get().plans.map((p) => (p.id === plan.id ? { ...p, ...plan } : p))
            : get().plans,
          error: null,
        });
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    },

    createPlan: async (params) => {
      try {
        const { plan } = await api.plans.create(params);
        await get().fetchPlans();
        await get().fetchPlan(plan.id);
        return plan;
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
        return null;
      }
    },

    updatePlan: async (planId, fields) => {
      try {
        await api.plans.update(planId, fields);
        await refreshAfterMutation(planId);
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    },

    removePlan: async (planId) => {
      try {
        await api.plans.remove(planId);
        const { [planId]: _removed, ...rest } = { ...get().steps };
        set({ steps: rest, plans: get().plans.filter((p) => p.id !== planId) });
        await get().fetchPlans();
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    },

    addStep: async (planId, params) => {
      try {
        await api.plans.addStep(planId, params);
        await refreshAfterMutation(planId);
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    },

    updateStep: async (planId, stepId, fields) => {
      try {
        await api.plans.updateStep(stepId, fields);
        await refreshAfterMutation(planId);
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    },

    removeStep: async (planId, stepId) => {
      try {
        await api.plans.removeStep(stepId);
        await refreshAfterMutation(planId);
      } catch (e) {
        set({ error: e instanceof Error ? e.message : String(e) });
      }
    },
  };
});
