# ADR-006: Local OCR for sensitive images

- Status: Accepted (direction finalized; OCR engine selection pending measurement)
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

In the demo (§7, 23:30-24:58) the customer raised a concern: if sensitive images
(containing national ID numbers, contracts, invoices) go through cloud OCR, the data leaves
the premises, carrying a leak risk. They asked for local OCR; if local OCR isn't reliable
enough, they'd rather block image upload outright than let sensitive images leave the
premises.

Current-state anchor: **the project has no OCR at all.** Grepping the entire `crates` tree
turns up no implementation of tesseract / mineru / `ocr`. The `allow_image_input`
capability flag doesn't exist yet either. The deny-by-default capability mechanism already
exists (`CapabilitiesConfig`, `crates/duduclaw-core/src/types.rs:471`) and can host a new
flag. `TODO-feature-gaps` §8 has already planned MinerU for PDF (via a Python bridge).

This is a technology-selection spike. The catch is that "which OCR engine has good enough
Traditional Chinese recognition" isn't a question a spec sheet can answer — it depends on
real sample images.

## Options considered

**(a) MinerU**
Already planned (queued for PDF in §8), layout-aware (can handle tables / multi-column /
mixed text-and-image layouts), high quality ceiling. The downside is that it's heavy — many
Python dependencies, large models, high startup and memory cost.

**(b) Tesseract subprocess**
Light, mature, cross-platform, cheap to install. The downside is that **Traditional Chinese
quality is unverified** — Tesseract's Chinese model is reasonable for printed text, but its
performance on tables, handwriting, low resolution, and vertical text is unknown; it can't
be assumed to be "probably fine" based on training data alone.

**(c) macOS Vision framework**
Native to Apple, with a reportedly good track record for Traditional Chinese recognition in
practice, and zero extra dependencies. Its fatal flaw is platform lock-in — it only runs on
macOS, and the gateway loses it entirely when deployed to Linux.

## Decision

**Do not pre-select an OCR engine.** First run a real recognition-accuracy test — **10
Traditional Chinese sample images** (covering real-world scenarios such as national IDs,
invoices, contracts, tables) — measure all three candidates' recognition rates, and decide
based on the numbers. The project's guiding principle is "don't guess blindly / measure
before choosing" — there is no conclusion on OCR quality without an actual measurement.

What to measure before finalizing the selection:
- Character-level recognition accuracy (especially for fields the redaction pipeline needs
  to catch, like digits and national ID numbers).
- Degree of layout destruction (whether tables / multi-column layouts get scrambled badly
  enough that downstream redaction can no longer catch them).
- Per-image latency and resource footprint (it needs to hold up running on-premise).

**Regardless of which OCR engine is chosen, the block-first fallback ships first anyway.**
The blocking gate in WP12-T12.1 is independent of the OCR engine, and it's cheap and
fail-closed: `agent.toml [capabilities] allow_image_input` (defaults to `true` for backward
compatibility); when set to `false`, each channel simply doesn't download images or let them
into context on receipt, and replies with a zh-TW explanation instead. The media-download
choke point differs per channel across all nine channels, so each is wired up and checked
off individually. This ships first, closing off the sharpest risk — sensitive images
leaking out — in the most conservative way possible, without waiting on the OCR selection.

The OCR path itself (T12.3) only gets wired up once the engine is selected: image → local
OCR → text flows through the existing redaction pipeline (WP2 rules apply directly) → only
then does it enter the LLM context. OCR failure with `allow_image_input=false` ⇒ blocked;
OCR failure with allow enabled ⇒ a config choice between two options
(`degrade: block | passthrough`, default block, fail-closed).

## Consequences

**Gained:** the selection is grounded in real numbers rather than spec-sheet guesswork; the
fail-closed block-first path — cheap and independently shippable — lands first, so the risk
of sensitive images leaking out is closed off immediately and isn't held hostage to the OCR
selection.

**Paid:** full OCR functionality has to wait for the 10-sample-image measurement to
complete before work starts — a deliberate delay. Better to choose correctly a bit later
than to choose wrong a bit sooner. All three candidates have a fatal weakness of their own
(MinerU is heavy, Tesseract's Traditional Chinese is unverified, Vision is platform-locked),
and none of them can be picked without looking at the numbers first.

**Delivery grading:** per WP12's Definition of Done, if OCR quality doesn't meet the bar,
shipping T12.1's blocking gate alone is enough to mark it PARTIAL. Block-first is the
smallest independently shippable and acceptable unit.

**Once selected:** once the OCR engine is decided based on the measured numbers, record
which one was chosen and on what data as an addendum to this ADR or in a new ADR. Until
then, this ADR's OCR-engine field stays "pending measurement" and carries no unmeasured
leaning of any kind.
