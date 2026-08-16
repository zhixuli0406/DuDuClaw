# ADR-005: Document export (md → Slide / Word / PPT / PDF)

- Status: Accepted
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

During the demo (7:35), the customer asked whether an agent can produce Google Slides or
Microsoft Office files. They acknowledged this area is complex and they're still
researching it.

Current-state anchor: **there is nothing today.** Grepping the entire Rust source turns up
no implementation of pptx / docx / pandoc / `docx-rs` / `rust_xlsxwriter`; the only hits for
the strings `pptx` / `docx` are in the compiled dashboard bundle (`PartnerPortalPage`), where
they're download links for partner marketing assets — unrelated to document generation.
ReportPage's export is a frontend JS function; it does not produce a file that can be sent.

An agent's output is markdown by nature. Turning it into a Slide / Word / PPT / PDF that a
customer can receive, open, and edit requires a conversion pipeline. This is a
technology-selection spike — settle the direction first, then start implementation
(mapping to the `document_export` MCP tool in WP11-T11.2).

## Options considered

**(a) Pure Rust: `docx-rs` / `rust_xlsxwriter`**
Zero external dependencies, clean single-binary packaging, consistent across platforms. The
downside is a weak pptx ecosystem — Rust has no mature pptx-generation library, and hand-
rolling OOXML is expensive. docx is feasible; pptx is the sore spot.

**(b) Pandoc subprocess**
md → docx is extremely mature; md → pptx is workable (Pandoc has a pptx writer). The
downside is an external binary dependency — Pandoc isn't guaranteed to be present on the
user's machine. This can be resolved with detect-then-enable: enable it when Pandoc is
detected, and fail-soft degrade to an md attachment when it isn't.

**(c) HTML → PDF (headless browser)**
The project already has Playwright MCP as a browser layer (L3), so in theory md → HTML →
render to PDF. PDF quality would be high with controllable layout. **Honest caveat on the
current state**: the gateway's `browser_router.rs` is currently a skeleton — actual browser
automation runs through Playwright MCP, not this router, and the full loop for PDF
rendering hasn't been wired end-to-end yet. It cannot be treated as "already there."

**(d) Google Slides API**
Native Google Slides, the closest fit for customers heavily invested in Google Workspace.
The downside is that it requires OAuth, routes data through the cloud, and carries higher
implementation and authorization-maintenance cost. It conflicts with the product's
on-premise-first direction.

## Decision

**md → docx / pptx goes through Pandoc (detect-then-enable, fail-soft degrade to an md
attachment); PDF goes through the existing browser layer. The pure-Rust path is kept as a
future option.**

Rationale: Pandoc covers the two most-requested formats, docx and pptx, in one shot, and
pptx is exactly where pure Rust falls short. Detect-then-enable turns "requires an external
dependency" into a graceful degradation — without Pandoc it falls back to an md attachment
with an explanatory note; it doesn't crash and doesn't pretend to succeed. Riding on the
browser layer is the shortest path to PDF and introduces no new dependency. Native Google
Slides involves OAuth and a cloud data flow, which conflicts with the on-premise-first
product direction, so it's not being done for now.

Implementation notes (details in WP11-T11.2):
- MCP tool `document_export`: takes md content + target format (docx / pptx / pdf) as
  input, writes the produced file into the agent workspace, and sends it out from the
  channel as a file message.
- Minimal pptx template: a title slide + bullet slides, styled with DuDuClaw's brand colors.
- Pandoc absent → fail-soft degrade to an md attachment (fail-open to the most conservative
  available output, not a silent failure).

## Consequences

**Gained:** a clear, mature path exists for the two major md → Office formats (docx / pptx);
environments without Pandoc don't break, they just get md; no OAuth or cloud data flow is
introduced.

**Paid:** Pandoc is a runtime external dependency, and the deployment docs need to spell out
how to install it — Office output only exists once it's installed. The browser layer that
PDF depends on is currently a skeleton; the PDF path only counts once the browser loop is
actually wired up — this point cannot be glossed over with the customer.

**Honest talking points for the customer:** "md → Office is now a supported direction
(docx / pptx); native Google Slides is still under evaluation." Don't promise Google Slides,
don't overstate the current state of PDF.

**Future option:** if the external dependency becomes a real pain point (e.g. a customer
wants a single binary and forbids installing Pandoc), enable the pure-Rust `docx-rs` path;
at that point, separately evaluate whether hand-rolling OOXML for pptx is worth it. Record
that pivot with a new ADR.
