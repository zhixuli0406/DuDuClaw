# UCCI calibrated cascade

DuDuClaw can use Varun Kotte's [UCCI: Calibrated Uncertainty for Cost-Optimal
LLM Cascade Routing](https://arxiv.org/abs/2605.18796) ([reference code](https://github.com/varunkotte6/ucci)) router files for
post-generation escalation. The Rust dependency is pinned to a commit, and
the router files use UCCI's versioned JSON format. The signal is UCCI's mean
top-2 token margin uncertainty, not DuDuClaw's earlier mean-logprob score.

## Collection and fitting

Enable the existing router and an observation file in `inference.toml`:

```toml
[router]
enabled = true
fast_model = "your-fast-model"
strong_model = "your-strong-model"
ucci_observations = "ucci/observations.jsonl"
ucci_drop_stop_token = true # for vLLM/llama.cpp if logprobs include EOS
ucci_shadow_strong = true    # opt-in: collect Strong answers for returned Fast replies
ucci_shadow_max_inflight = 1 # cap on concurrent background shadow generations (default 1)
```

Only the OpenAI-compatible inference backend currently supplies the needed
top-2 logprobs. Collection forces temperature 0. If the server does not return
two candidates per content token, `u` is null and no calibrated decision is
made.

Since 2026-09-29 that backend no longer carries its own HTTP client: the
`logprobs` / `top_logprobs` request fields and the per-token response parse are
part of the shared `duduclaw-llm` OpenAI-compat provider, and the local backend
is a thin shell over it that keeps the two local-specific settings (a 300s
request timeout, and model ids sent verbatim so a server's `qwen/qwen3-4b` is
not mistaken for a `provider/model` qualifier). The UCCI margin computation
itself deliberately stayed on the inference side — the shared provider carries
the signal, it does not score it — so `ucci_fit.py` / `ucci_pair.py` input
formats are unchanged. The observation file contains prompts and answers in plaintext; it is
opt-in. Each row has a shared `request_id` for the cascade, a unique `id`,
`stage`, `u`, the answer, model, latency, and empty human-label fields.

For Fast → Strong, `ucci_shadow_strong` supplies the second answer. The Strong
generation runs **in the background**: the Fast reply is returned as soon as it
is accepted and no longer waits for a second model call, so turning collection
on costs throughput on the host rather than latency for the user. The
consequence is that an observation row may be appended **after** the reply it
describes, and rows for concurrent requests may interleave — pair rows by
`request_id`, never by file order (`scripts/ucci_pair.py` already does). Each
append still holds a cross-process advisory lock, so a row is never truncated
or interleaved mid-line. A process that exits while a shadow is still
generating loses that last row and nothing else; `InferenceEngine::flush_shadow_observations()`
drains whatever is in flight for an embedder that wants to wait.

Detaching the shadow onto a background task means it can now overlap with the
*next* request's own foreground generation, contending for the same
backend/model slot. `ucci_shadow_max_inflight` (default `1`) bounds how many
shadow generations may run concurrently: a new shadow attempted while the cap
is already saturated is **skipped** — never queued, never blocking the
foreground reply — and the skip is counted and `debug!`-logged. Raise it only
if the backend can genuinely serve overlapping generations (e.g. a batching
server); `0` is treated the same as `1`, not as "unbounded".

Build a manual-review template with:

```sh
python3 scripts/ucci_pair.py --observations observations.jsonl \
  --stage local_fast --out fast-review.jsonl
```

For Strong → Cloud, collect Cloud answers separately with the same
`request_id` and `stage = "cloud_api"`, then pass
`--cloud-observations cloud.jsonl --stage local_strong` to the same helper.
Review **both** answers manually and fill the label fields:

```json
{"id":"example-local-fast","stage":"local_fast","u":0.42,"answer":"local answer","large_answer":"strong answer","small_correct":0,"large_correct":1,"label_source":"human","split":"cal"}
```

Use `stage = "local_fast"` for Fast → Strong, and `stage = "local_strong"`
for Strong → Cloud. Do not use the router's `escalated` decision or an LLM
judge verdict as an accuracy label. The latter may be kept separately for an
audit. Include examples regardless of whether the current gate would have
escalated them; selecting only escalated examples biases the fit. Keep
calibration, validation and test examples disjoint. The helper accepts either
`answer` or `small_answer` for the local reply.

Install `ucci-router`, then fit each stage with a **chosen accuracy target**
and **measured per-call costs**:

```sh
python3 -m pip install ucci-router==0.1.1
python3 scripts/ucci_fit.py --data reviewed.jsonl --stage local_fast \
  --tau 0.90 --c-small 1 --c-large 3 --out fast-router.json
python3 scripts/ucci_fit.py --data reviewed.jsonl --stage local_strong \
  --tau 0.95 --c-small 3 --c-large 12 --out strong-router.json
```

The values above illustrate the command shape; choose targets and costs from
your workload. The helper rejects non-human labels and rows without paired
answers. It invokes UCCI's `fit` with `--cost-model sequential`, because
DuDuClaw runs the current model before deciding to pay for the next one.
UCCI uses a separate calibration split to fit the isotonic map, a validation
split to choose the threshold, and a test split for final evaluation. Run
`ucci evaluate --router fast-router.json --data fast-router.json.reviewed.jsonl --split test`
and the analogous `ucci report` command before enabling the fitted files.
The helper saves the label-only input beside each router so UCCI can reproduce
the split and verify its data digest. Keep the original reviewed answers and
model version with each file.

## Serving

After validating the fitted files, set:

```toml
[router]
enabled = true
ucci_fast_router = "ucci/fast-router.json"
ucci_strong_router = "ucci/strong-router.json"
ucci_observations = "ucci/observations.jsonl"
ucci_drop_stop_token = true
local_tools = false # for a dedicated bare-completion calibration trial
```

Relative paths resolve from the DuDuClaw home directory. A router is loaded
only if its file is valid and its cost model is `sequential`. Each local tier
uses its own fitted router. UCCI escalates when its calibrated error
probability is **strictly greater** than that tier's threshold. A missing
file, missing top-2 signal, or unsupported backend escalates from a
**configured** tier; a tier without a UCCI file accepts its local answer.
Check warnings and observation rows before treating the calibrated gate as
active. UCCI is now the only calibration gate in the router: the legacy
`post_hoc_enabled` / `post_hoc_alpha` / `post_hoc_beta` /
`post_hoc_accept_threshold` logistic settings were removed on 2026-09-29
(`wiki/reports/feature-audit-2026-09-29.md` T3-S7) because their defaults were
never fitted — with alpha 4.0 / beta -2.0 / threshold 0.5 the "probability"
was a fixed cutoff at mean logprob >= ln 0.5, and nothing persisted the score
next to an outcome label. Those four keys are ignored if left in
`inference.toml`; a tier with no UCCI file simply accepts its local answer.

## Current boundaries

The gateway's MCP tool loop uses a different provider path and does not pass
through `InferenceEngine::route_and_generate`; its replies are outside this
calibrated gate. Use a dedicated bare-completion workload with
`local_tools = false` to evaluate the fitted gate; tool-requiring tasks need
their existing tool-capable path. `ucci_shadow_strong` records Strong answers for Fast replies
that would otherwise be returned, in the background and possibly after the
reply lands (see above). The observation file does not run Cloud
shadow answers or attach outcome labels. Collect Cloud outputs separately
for Strong → Cloud validation. A two-stage cascade also needs stage-specific
data: a Strong
→ Cloud fit trained on all Strong requests can differ from the population
that reached Strong after the Fast gate.
