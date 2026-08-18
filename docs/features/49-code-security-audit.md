# Code security audit

> Point `duduclaw secaudit` at a repository and it finds security problems the
> way a careful reviewer would: run the fast scanners, have an agent read the
> risky modules, then make a second agent try to disprove each finding before
> it reaches you.

## What it is

Static scanners are fast and certain but miss anything that needs reasoning
across files — a tainted value that flows three modules before it's used, a
state machine that can be driven into a bad state, a business rule that only
breaks under a specific sequence. A language model can follow those paths but
invents plausible-sounding bugs that aren't real. `duduclaw secaudit` runs
both and makes them check each other.

It reuses machinery the platform already has: the multi-runtime agent layer
does the deep reading, the container sandbox runs proof-of-concept code with
no network, and the same maker-checker discipline used elsewhere becomes an
adversarial review pass. The one piece it doesn't build is a static engine —
it orchestrates the scanners you already trust (gitleaks, semgrep,
cargo-audit, osv-scanner) rather than reinventing them.

## The pipeline

1. **Intake / threat modeling** (deterministic, no LLM). Profiles the repo —
   language mix, entry points — and mines git history for modules that see
   frequent security-related commits, so the expensive steps focus on the
   risky areas first.
2. **Static scan.** Runs whichever scanners are installed and normalizes their
   output into one finding shape. A scanner that isn't installed is reported
   as missing with the reason, never silently skipped.
3. **AI deep audit** (`--profile deep`). Reads each ranked module under a
   fixed prompt budget and proposes concrete vulnerabilities a static tool
   would miss. Bounded by `--max-modules` so it can't run away.
4. **Adversarial review.** Each AI candidate gets a fresh agent with none of
   the first pass's context, told to disprove it from a clean re-read of the
   file: does the code really exist, is the path really reachable? Refuted
   candidates stay in the report but are marked as such; plausible ones are
   parked for a human, never auto-confirmed. Static-scanner findings skip this
   step — they're deterministic evidence already.
5. **Proof of concept** (`--poc`, High severity and above only). Generates a
   PoC and runs it inside the container sandbox (no network, tmpfs, hard
   timeout). If no container runtime is available it records the PoC as
   skipped — it never runs on the host.

## Running it

```
duduclaw secaudit .                                   # quick: scanners only
duduclaw secaudit . --profile deep --max-modules 5    # + AI audit & review
duduclaw secaudit . --poc --fail-on high --save       # + sandboxed PoC, save report
```

Exit code is `0` when nothing meets `--fail-on` (default `high`), `1` when
something does, and `2` only for an infrastructure error — a machine with no
scanners installed still exits `0`, so it drops cleanly into CI. Refuted and
suppressed findings stay visible in the report but don't count toward the
severity stats or the gate, so adversarial review actually lowers the noise
that fails a build.

`--save` writes the report to `<home>/secaudit/reports/`, where the dashboard
picks it up.

## Reviewing in the dashboard

The Security audit page lists saved reports and, for each one, groups findings
by severity with an expandable evidence chain (the static hit, the AI
reasoning, the adversarial verdict, any PoC transcript). Each finding carries
three reviewer actions — confirm, suppress, refute — that write back to the
report. The page is manager-gated.

## What's deliberately left to you

The audit never marks a finding "confirmed" on its own — a plausible one waits
on the Security audit page for your decision. Reporting a vulnerability
upstream (if you audit an open-source dependency) is an outward-facing action
and stays a human step.
