# Calibrated forward model and held-out learning gate

> Every guess an AI teammate makes gets scored against reality — and self-derived lessons stay on the bench until the numbers back them.

---

## What this is

Since DuDuClaw 1.54, any AI teammate can put a probability on "will this step succeed?" before acting. Afterwards, the system takes what the tools actually returned and grades that number with statistically fair scoring rules. The grade does more than feed a chart: it decides whether a lesson the agent derived on its own may enter future prompts. A lesson either comes with tool records or audit evidence and is adopted directly, or it starts as a candidate whose hit rate on later real cases must beat a baseline before promotion. The whole capability ships enabled by default since v1.54; each layer can be turned off in the settings, and a disabled layer behaves exactly as the system did before the feature existed.

---

## Three problems this solves

**First, small samples invite self-confirmation.** An AI teammate handles thirty jobs, looks back for patterns, and easily mistakes noise for signal — writing it down as a rule, then making the next decision by that rule, which no data outside those thirty jobs has ever tested. The loop grows more confident with every turn, but no more accurate.

**Second, bolt-on "predict, act, verify" loops usually verify against their own logs.** The loop looks complete on paper, but the "verify" step never touches an external fact, which makes it no different in substance from a bare model answering one-shot questions.

**Third, nobody lines up the original guess with what actually happened and scores the gap.** Without that comparison, a model never becomes more accurate for having guessed wrong — it only sounds more convincing for speaking fluently.

---

## How it works

Three stages, all pure arithmetic, no extra LLM calls:

```
Before acting
     |
     v
+----------------------+
|  Commit a guess      |  <-- TaskPrediction.confidence is recorded
+--------+-------------+      before the action; immutable afterwards
         |
         v
+----------------------+
|  Score with external |  <-- actual tool results are the referee;
|  evidence            |      Brier score or RPS
+--------+-------------+
         |
         v
+----------------------+
|  Murphy              |  <-- only rising resolution counts
|  decomposition       |      as real learning
+----------------------+
```

### Predict before acting

Before the action, the task-level prediction (`TaskPrediction.confidence`) records a probability: will this step succeed, how long will it take, and if it fails, which failure class is most likely. The number is written down before the action happens and cannot be revised afterwards — commit first, so the claim can later be refuted.

### Score with external evidence

When the task ends, the system uses what the tools actually returned (not what the AI teammate says happened) as the referee, computing a Brier score (binary judgments) or RPS (three-way ordered judgments). Both scores are bounded, fixed to the 0–1 range. The widely used log score is deliberately avoided here: one absurd prediction can drag the total to infinity, which makes it unstable on small samples.

### Check for real learning with the Murphy decomposition

A Brier score splits into three parts: reliability, resolution, and uncertainty. The key judgment lives here: only resolution rising over time counts as having learned a predictive pattern. Reliability improving while resolution stays flat means the AI teammate has merely learned to report the average (always guessing "fifty-fifty" is the safest way to avoid penalty — and learns nothing). The system tracks the two numbers separately and never looks at the total alone.

The baseline being competed against must be frozen. It cannot drift along with the data, or "beating the baseline" itself becomes untestable.

---

## Honest labels

The system permits only three conclusions:

| Label | Meaning |
|-------|---------|
| `SUPPORTED` | Out-of-sample performance is statistically better than the baseline |
| `CANDIDATE` | Not enough samples yet; no verdict for now |
| `INDISTINGUISHABLE_FROM_LUCK` | The confidence interval straddles fifty-fifty, or the calibrated win-rate confidence falls short — mathematically, skill and luck cannot be told apart |

"We don't know yet" is a legitimate and common conclusion. The system will not force a vague "preliminarily effective" just to look useful.

---

## The held-out learning gate: reflection proposes, numbers dispose

Self-reflection (reflexion) still produces candidate lessons, but every lesson passes through a classification first:

```
Reflection produces a candidate lesson
        |
        v
Backed by tool records or audit evidence?
        |
   +----+----+
   |         |
  yes        no
   |         |
   v         v
Adopt      Shadow candidate (never injected)
directly        |
                v
          Each new real case logs a hit or a miss
                |
                v
          Wilson lower bound vs. the baseline
          (Bonferroni-corrected when several
           candidates compete at once)
                |
           +----+----+
           |         |
          wins     doesn't
           |         |
           v         v
        Promoted,  Keeps observing;
        starts     repeated misses
        injecting  retire it
```

- **Lessons verifiable from tool records or the audit log** (for example, a particular tool-parameter format error causing failures) are adopted directly, as before — the basis is settled fact.
- **Lessons with no programmatic evidence, derived purely by induction** (for example, a statistical hunch that success rates run lower in some situation) start as shadow candidates and never appear in prompts. Each new case afterwards logs whether the candidate guessed right or wrong. Once enough samples accumulate, the Wilson confidence-interval lower bound (tightened by Bonferroni correction when several candidates compete at once) is compared against the baseline. Only beating the baseline earns promotion and injection. If a promoted lesson later regresses and its rolling-window lower bound falls back below the baseline, it is demoted to candidate immediately — the record is kept, never deleted.

---

## How candidates graduate: shadow scoring

Both promotion paths for candidate lessons are wired up. On the autonomous task-loop side, every task settlement logs a hit or a miss for each candidate whose context matches. On the ordinary conversation side, prompt assembly first registers the candidates whose trigger signals match the turn's context (still without injecting them), then settles the books for every registered candidate once the reply's outcome lands.

Both sides use the same yardstick. A candidate's implicit prediction is "risk runs high in this situation": a real incident counts as a hit, a false alarm as a miss. Once samples reach the threshold, the Wilson lower bound (Bonferroni-corrected when several candidates compete) is compared against the teammate's own historical base rate — winning earns promotion and injection, while candidates that keep producing false alarms retire. A promoted lesson that regresses is pulled back to candidate, its record kept, free to accumulate fresh samples and climb back the next time its signals match. Base rates are computed per layer: the task layer reads task-settlement records, the conversation layer reads conversation-error records, and with fewer than eight samples of history both fall back to fifty-fifty — conservative on purpose.

---

## How to enable

All three switches live under `[task_forward_model]` and stack layer by layer:

```toml
# config.toml (global). Also switchable in the dashboard under
# Advanced settings → Prediction calibration.
[task_forward_model]
enabled = true                # master switch: task-level prediction itself, default true since v1.54
calibration_enabled = true    # scoring: Brier/RPS + Murphy decomposition, default true since v1.54
held_out_gate_enabled = true  # learning gate: reflexion candidates need out-of-sample validation, default true since v1.54
```

The three layers stack independently. `enabled` records predictions; `calibration_enabled` starts scoring and produces honest labels; `held_out_gate_enabled` decides whether self-learned lessons may be injected. All three default to on since v1.54. Setting any layer to `false` makes that layer behave byte-identically to the system before this feature existed, so the layers can be switched off one at a time.

---

## Not tied to any one kind of agent

The mechanism is deliberately business-agnostic: internally the engine only understands abstract `(confidence, realized outcome)` pairs and knows no domain vocabulary. As an example, a coding agent that edits its own code predicts the probability that "the tests will pass after this change" before a risky refactor; the test result afterwards is the external referee. If it induces "whenever this particular error code appears, running the formatter first makes the retry pass," that lesson is verifiable from tool records and adopted directly. But if it induces "code changed on Fridays breaks more often" — a statistical hunch with no programmatic evidence — it must start as a candidate, accumulate samples, and beat the baseline before it is ever used. The same engine works identically for customer-support, trading, or any other kind of AI teammate.

---

## Dashboard view (added 2026-08-13)

The memory page gains a "prediction calibration" tab — the feature's first dashboard surface. Per AI teammate it shows: total predictions / settled count / mean error score (Brier, lower = more accurate), the four-level outcome distribution (as expected / small deviation / clear deviation / severe deviation), observation-fidelity and prediction-source distributions, plus a "recent predictions vs. outcomes" comparison list. The data source is the `task_prediction_log` audit trail in `prediction.db` (read-only RPCs `forward.summary` / `forward.recent`); the statistics window is bounded (most recent 5,000 entries) and honestly labeled in the UI. The surface is generic: any teammate with task prediction enabled appears here, with no tie to any particular experiment or industry.

---

## Research grounding

- MuZero (arXiv:1911.08265): a world model need not reconstruct full environment dynamics; predicting the quantities that affect decisions (success, cost, failure class) is enough.
- Gneiting & Raftery, *Strictly Proper Scoring Rules* (JASA 2007): the definition of proper scoring, and why bounded scores are steadier than the log score on small samples.
- Murphy (1973) / Siegert (*QJRMS* 2017): the three-way Brier decomposition, reliability − resolution + uncertainty — the direct basis for this feature's calibration criterion.
- Richens et al., ICML 2025 (arXiv:2506.01622): any mechanism that genuinely improves multi-step task performance must, mathematically, carry extractable and testable world-model content. This is the theoretical basis for claiming a calibration loop here without claiming a pretrained world model.
- RSEA (arXiv:2606.28374): online self-evolution without a held-out gate collapses — the paper's Dynamic Cheatsheet dropped from 70.7% to 0.14% accuracy after a scenario change. The direct motivation for the held-out learning gate.
- 2310.01798: self-correction with no external signal, relying purely on the model grading itself, is ineffective or worse — the basis for scoring only with external tool results and never using LLM self-assessment as grounds for adoption.

---

## Related documents

- Evolution v3 (the playbook learning container): [`38-aee-playbook-evolution.md`](38-aee-playbook-evolution.md)
- Evolution switches overview: [`../guides/evolution-switches.md`](../guides/evolution-switches.md)
