# Code security audit

> Point `duduclaw secaudit` at a repository and it finds security problems the
> way a careful reviewer would: run the fast scanners, have an agent read the
> risky modules, then make a second agent try to disprove each finding before
> it reaches you.

## What it is

Static scanners are fast and certain but miss anything that needs reasoning
across files: a tainted value that flows through three modules before it's
used, a state machine that can be driven into a bad state, a business rule
that only breaks under a specific sequence. A language model can follow those
paths but invents plausible-sounding bugs that aren't real. `duduclaw secaudit`
runs both and makes them check each other.

It reuses machinery the platform already has: the multi-runtime agent layer
does the deep reading, the container sandbox runs proof-of-concept code with
no network, and the same maker-checker discipline used elsewhere becomes an
adversarial review pass. The one piece it doesn't build is a static engine.
It orchestrates the scanners you already trust (gitleaks, semgrep,
cargo-audit, osv-scanner) rather than reinventing them.

Version 2 of the report adds what a reviewer needs to trust a run: a
structured claim for every AI finding, a check that throws out fabricated
ones without spending a model call, a record of which modules were actually
read, and an honest label when the run did not finish.

## The pipeline

1. **Intake / threat modeling** (deterministic, no LLM). Profiles the repo
   (language mix, entry points) and mines git history for modules that see
   frequent security-related commits, so the expensive steps focus on the
   risky areas first.
2. **Static scan.** Runs whichever scanners are installed and normalizes their
   output into one finding shape. A scanner that isn't installed is reported
   as missing with the reason, never silently skipped.
3. **AI deep audit** (`--profile deep`). Reads each ranked module under a
   fixed prompt budget. The model must answer in one strict JSON array; a
   reply with prose around it, or two JSON values, voids that module's
   answer instead of being patched up. Every candidate has to fill in:
   - a **threat model** with six slots: `principal` (who attacks), `input`
     (what they control), `control` (the check that should stop them),
     `boundary` (the trust line crossed), `affected` (what is hurt) and
     `result` (what happens). A blank or invisible-text slot fails the
     candidate.
   - a **trace** from an `entrypoint` through any `propagation` steps to a
     `sink`, each step with file, line and scope.
   - the **conditions** the attack needs (authentication level, user
     interaction, system configuration and similar).

   The prompt also carries severity anchors and a list of anti-patterns, so
   "this function looks dangerous" doesn't get reported as High. Severity is
   about impact; certainty is tracked separately by the review steps below.
4. **Deterministic pre-check** (no LLM). Before any review call, each
   candidate is checked against the repository itself: the file path must be
   a safe relative path that belongs to the module that was read, the line
   must exist in that file, every trace step must point at a real file and
   line, and the six threat-model slots must contain visible text. A
   candidate that fails is refuted on the spot, with the violations recorded,
   and never reaches the reviewer. This is what catches invented paths and
   line numbers.
5. **Adversarial review.** Each surviving candidate goes to a fresh agent with
   none of the first pass's context, told to disprove it. The reviewer now
   sees the whole picture it needs: a large window of the main file (up to
   24 KiB around the reported line), the lines around every other trace
   step, the threat model and the conditions. It answers `refuted` or
   `plausible`. A `plausible` verdict must name what blocks automatic
   confirmation (`blockers`) and say how a human could validate it
   (`validation_plan`, locally and/or in a deployment); without those the
   verdict is rejected and the candidate stays a candidate. Static-scanner
   findings skip this step, since they are deterministic evidence already.
6. **Proof of concept** (`--poc`, High severity and above only). Generates a
   PoC and runs it inside the container sandbox (no network, tmpfs, hard
   timeout). If no container runtime is available it records the PoC as
   skipped and never runs it on the host.

## What the report tells you about coverage

Every module the ranking produced gets one coverage entry, including the ones
`--max-modules` left out. Status is one of: `covered` (read, no candidates),
`candidate` (read, at least one candidate), `deferred` (not reviewed because of
`--max-modules` or the candidate cap), `unreadable`, `llm_failed` or
`parse_failed`. The summary prints a line such as "partial coverage: N modules
not reviewed" whenever any module was not read. A green result on a partially
covered repo means "nothing found in what was read", and the report says so.

The run itself has a `run_status`: `complete` or `incomplete`. An incomplete
run carries an `incomplete_reason`, for example `engine_unavailable` (the AI
engine failed on its first call) or `validation_budget_exhausted` (the
candidate cap was reached). The quick profile has no AI steps and is always
complete.

## Carrying results forward

By default a run reads the newest saved report for the same repository (from
`<home>/secaudit/reports/`) and compares findings by a stable fingerprint
that ignores line numbers, snippets and severity, so a finding survives code
moving around it. The carry-over only applies when the file's content hash is
unchanged:

- previously suppressed or refuted and the file is the same: the finding keeps
  that verdict, with no model call spent;
- previously confirmed and the file is the same: it is reviewed again, and the
  report counts it as a re-validated prior confirmation;
- the file changed: nothing is carried, and the report counts it.

A prior report that cannot be read is skipped with one line on stderr.
`--no-prior` turns the whole mechanism off. The report's `prior_run` block
says which file it carried from and how many findings of each kind.

## Running it

```
duduclaw secaudit .                                   # quick: scanners only
duduclaw secaudit . --profile deep --max-modules 5    # + AI audit & review
duduclaw secaudit . --poc --fail-on high --save       # + sandboxed PoC, save report
duduclaw secaudit . --profile deep --verifier-agent reviewer-bot --fail-on-needs-human
duduclaw secaudit . --profile deep --no-prior         # ignore earlier reports
duduclaw secaudit-validate report.json                # check a saved report
```

Exit code is `0` when nothing meets `--fail-on` (default `high`), `1` when
something does, and `2` for an infrastructure error. A machine with no
scanners installed still exits `0`, so it drops cleanly into CI. Refuted and
suppressed findings stay visible in the report but don't count toward the
severity stats or the gate, so adversarial review lowers the noise that fails
a build.

**Findings parked for a human.** A `plausible` verdict leaves the finding as
`needs_human`. Its severity is whatever the model reported about itself, and
the report labels it `model_self_reported`. These findings are counted in a
separate `needs_human_by_severity` table, shown apart in the dashboard, and
do **not** fail the build. If you want them to, add `--fail-on-needs-human`:
the gate then reads both tables. The reasoning is that a model's guess about
severity is not evidence, so by default it cannot break your pipeline, but you
can choose to be strict.

**Who reviews.** By default the reviewer runs as the same agent that did the
deep audit, which means the same model may be marking its own work. Pass
`--verifier-agent <id>` to run the review and PoC steps as a different agent
(its own runtime and model). The report's `verifier` block records
`independence` as `same_agent`, `different_agent` or `not_run`, and names both
agents. The tool does not force a different model; it makes the choice visible.

**Report validation.** Before a report is written (`--report` or `--save`), a
validator checks it: unique ids, safe file paths, a refuted finding has its
review evidence, a `needs_human` finding has blockers, summary counts match the
findings, every coverage entry points at real findings, an incomplete run
states why. If the validator finds a violation it prints them and exits `2`
rather than writing a bad report. `duduclaw secaudit-validate <report.json>`
runs the same checks on a saved file: exit `0` valid, `1` violations (listed
one per line), `2` unreadable or not JSON. Reports from before v2 have no
`schema_version`, count as version 1, and are not accepted by the validator.

`--save` writes the report to `<home>/secaudit/reports/`, where the dashboard
picks it up.

**Security fix.** Earlier versions took the file path the model returned in a
candidate and joined it onto the repository root without checking it. In deep
mode (the adversarial review and PoC steps) a path such as an absolute one
could therefore point the read outside the repository. Every model-returned
path is now validated (relative only, no `..`, no drive or UNC prefix, no
control characters, no Windows reserved names) before any file is opened.

## Reviewing in the dashboard

The Security audit page lists saved reports. Open one and you see, at the top:

- an **incomplete banner** with the reason, when `run_status` is `incomplete`;
- a **coverage card**: modules in total, covered, candidate, deferred and
  failed, marked "partial coverage" when any module was not read;
- a **verifier chip** reading "same agent review" or "independent agent
  review", and a **prior-run chip** when results were carried forward.

Findings are grouped by severity, with a separate row below the counts for
"needs human judgment (model self-assessed, not counted in fail-on)". Each
finding expands into its evidence chain (the static hit, the AI reasoning, the
adversarial verdict, any PoC transcript) and, for AI findings, the six
threat-model slots, the trace, the conditions, the blockers, the validation
plan and any pre-check violations. A "model self-assessed" note sits next to a
severity the model chose itself. Each finding carries three reviewer actions
(confirm, suppress, refute) that write back to the report. The page is
manager-gated. A v1 report with none of the new fields still opens normally.

## What's deliberately left to you

The audit never marks a finding "confirmed" on its own. A plausible one waits
on the Security audit page for your decision. Reporting a vulnerability
upstream (if you audit an open-source dependency) is an outward-facing action
and stays a human step.
