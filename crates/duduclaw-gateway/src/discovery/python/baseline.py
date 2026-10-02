"""baseline-parallel-refine (SPEC §8).

plan_grid: the hard caps (in replay, min(hard cap, trace support)).
solve: open every legal root in batches of max_parallelism, then repeatedly
probe every legal action (all frontiers) in batches of max_parallelism until
nothing is legal. Ignores beta.
"""


class OptimalPolicy:
    NAME = "OptimalPolicy"
    POLICY_ID = "baseline-parallel-refine"

    def __init__(self, config):
        self.config = dict(config or {})
        self.beta = 0.6  # reported default; the baseline never reads it

    def plan_grid(self, context):
        w = int(context["hard_max_branch_count"])
        r = int(context["hard_max_refine_count"])
        if context.get("trace_branch_count") is not None:
            w = min(w, int(context["trace_branch_count"]))
        if context.get("trace_refine_count") is not None:
            r = min(r, int(context["trace_refine_count"]))
        return {"branch_count": w, "refine_count": r, "reason": "fixed baseline"}

    def solve(self, question, budget=None):
        p = int(question.max_parallelism)
        roots = list(question.legal_roots())
        for i in range(0, len(roots), p):
            question.probe_batch(roots[i:i + p])
        while True:
            actions = list(question.legal_actions())
            if not actions:
                return
            for i in range(0, len(actions), p):
                question.probe_batch(actions[i:i + p])
