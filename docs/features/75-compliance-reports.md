# Enterprise compliance reports

An Admin with an active Enterprise edition can use **Audit Logs → Compliance report** to download a record of outbound-gate decisions, approvals, action reviews and recorded data-source accesses for an internal personal-data review. Reports organize the available evidence for Taiwan Personal Data Protection Act review; they do not certify legal compliance.

## Generate a report

1. Open **Audit Logs**, then **Compliance report**.
2. Choose the start and end dates. Both dates use the Taipei calendar; the end date includes the whole day. The report uses UTC timestamps and the interval `[from, until)`. A range can cover at most 366 days.
3. Choose CSV, Excel (`.xlsx`) or printable HTML. Open the downloaded HTML in a browser and use **Print → Save as PDF** when a PDF is needed. The report labels follow the dashboard language: Traditional Chinese, English or Japanese.
4. Leave **Include unmasked personal data** unchecked for the default masked output. An Admin may explicitly select it to include the original values that remain in the source records. Both masked and unmasked exports are audited; an audit failure refuses delivery.
5. Select **Generate and download**. The 10,000-row limit applies across all report sections; one approval can appear in both the outbound and approval sections and count as two rows. Requests that exceed the row or data-volume limits are refused rather than silently truncated. Shorten the range and retry.

The button is hidden from non-Admins and Personal edition. The gateway also checks current Admin authority and the resolved edition; changing the page URL or calling the RPC directly does not bypass those checks. There is no employee MCP tool for generating this report.

## Read the evidence

The report includes summary counts and separate sections for outbound decisions, approvals, action reviews and recorded data-source tool calls and detection/masking events. The access section is not a complete access inventory. Recorded `db_select`, `db_query` and `db_tables` calls contribute metadata only; SQL and query results are not included. Outbound rows carry the recorded time, employee, destination channel or host, data categories, decision, approver and reason. Approval rows carry the kind, requester, decider, decision and timestamps. Missing fields stay blank. Approvals are snapshots of their current state, not a reconstruction of past state. A decision outside the selected interval is marked `decision_outside_range`; one without a recorded decision time is marked `decision_time_unrecorded`. Neither label infers the decision that applied during the selected interval.

**A gate decision is not a delivery receipt.** An approval can expire, a tool can fail later, and a refused request never establishes that data left the system. The report does not reconstruct missing history, identify unrecorded accesses or infer categories and approvers from message contents. A source that was not recorded, was rotated away or was removed by the source's retention policy cannot appear. Check the report's source and retention notes before interpreting a zero count.

Default masking assigns consistent pseudonyms to person fields within each report and masks the entire destination and reason fields. Other text is processed by the existing redaction engine with the `general` and `taiwan_strict` rule sets. Detection has limits; unknown or transformed personal data may remain. The explicit full-value option still masks credentials and cannot recover values already masked before storage. Reports do not contain or reconstruct the original private-data payload.

## Retention and handling

The report carries a retention note. Generating it does not extend the source records' retention, create a new legal basis for processing, or delete the source records. The generated file is downloaded to your browser; apply your organization's access controls, purpose limitation and retention policy to that copy. An unmasked report needs the same handling as its source personal data.

The Admin RPC is `compliance.generate` with required RFC3339 `from`, `until` and `format` (`csv`, `xlsx`, `html`), optional `language` (`zh-TW`, `en`, `ja-JP`, default `zh-TW`) and optional `include_personal_data` (default `false`). The upper boundary is exclusive. A successful response contains a filename, content type, base64 file content and counts; no persistent shared attachment is created.

See [Security defense](05-security-defense.md) and [Outbound data guard](69-egress-guard.md) for the enforcement boundaries that produce this evidence.

Report readers and audit writers share a cross-process lock across rotation and append, so a rotation during export cannot omit the live generation. Both retained tool-audit segments are verified from the same bounded snapshots used for the report; a broken hash chain, malformed record or read/lock failure refuses the export. Valid legacy prefixes remain readable but are not independently authenticated. Hash chains cannot prove tail completeness or authenticity against an external anchor.
