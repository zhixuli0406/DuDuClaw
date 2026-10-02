# Real S0 cp26 shared round 1

Source: Dream-RSI S0 `runs/cp26/shared/r1`, completed 2026-09-30T08:24:12Z.
This is the actual circle-packing n=26 run: 16 real Claude CLI exploration calls,
4 branches, root plus 3 refinements each; the earlier 4-call pilot is excluded.
Valid solutions: 13; timeout nodes: 3.
Recorded known cost: USD 3.6322657; 3 calls
have unknown final billing. Their `cost.usd = 0.0` is the original unknown
placeholder, not evidence of zero cost.
Model: `claude-haiku-4-5-20251001`. All recorded scores, costs, timestamps, sequence
numbers and visible sets are preserved. Sequence numbers start at the original
run-wide offset after the pilot. No synthetic node or replacement score is used.

The portable header keeps the standard 10 fields. Workspace paths are relative;
model proposal/transcript text and host-specific fields are omitted. These paths
are metadata only; this fixture does not contain the exploration workspaces.

`expected.json` is produced by the Python built-in baseline over all five beta
values. `script.json` fixes cells and legal batches for partial-reveal coverage;
`expected_script.json` is the independent Python replay result for that script.
All files are exported without mutating the source run or making API calls.

Source SHA-256:

- `world.json`: `030a0867c3046897039dd6f37cbcfa3a4cf12449dd745f34f7dc68004804b82f`
- `tree.jsonl`: `d173bb2a982af6023aea640afd63a4d97529c38eacb4775c040ccabbd595721f`
- `live_cycle_manifest.json`: `7c4345c6dcad75d60fa32b0775ee99103f48bb55a225c7cc02bb7849c1f4c71c`
