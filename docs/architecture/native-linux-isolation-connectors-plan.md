# Protected isolation: Personal bindings and production connectors

Status: implementation plan only, 2026-11-05. This document does not enable a
connector or change binding authorization. Source coverage, fixture tests,
production boot, native execution, and real provider acceptance are separate gates.

## Existing implementation

- `crates/duduclaw-mcp-boundary/src/bindings.rs` implements durable Personal and
  Shared lifetime bindings, principal generations, membership and connector
  checks. Its standalone store and tests are not the production lifecycle owner.
- `crates/duduclaw-gateway/src/isolation_mcp/authority.rs` rechecks confirmed
  employee allocation, tenant, user access, credential epoch, policy epoch and
  live run/control identity. Its `identity` and `check_epoch` accept Shared only;
  `authorize_connector` refuses every account. The human issuer in
  `isolation_mcp/boot.rs` constructs a Shared binding from the current tenant.
  An EmployeeScope grant does not remove these use-time checks.
- `isolation_connectors.rs`, `isolation_connector_registry.rs` and
  `isolation_connector_boot.rs` provide catalog selection, bound execution and
  complete per-tool registry checks. `InstalledConnectorProviders::assemble`
  accepts explicit providers and a provisioned binding store. This is a
  composition API, not proof that normal gateway boot installed its inputs.
- Native Google/Notion/GitHub, Odoo, database, channel, model, computer-login and
  search adapters exist under `crates/duduclaw-gateway/src/isolation_connector_*`.
  Google has a sealed owner in `google_identity/isolation_bridge.rs` and
  source-specific credential/consent handling. CLI-owned business executors
  remain under `crates/duduclaw-cli/src/mcp_isolation_connector_*.rs`.
- `BootSources` requires catalog, execution and selection producers; missing
  inputs or incomplete per-tool coverage refuse protected startup. Discovery
  doubles in parity tests prove no provider operation or credential selection.

## Missing production authority

Read the immutable Personal owner or Shared tenant from the trusted lifecycle
store used to allocate the employee. Neither employee TOML, a requester string,
the presence of a credential nor a synthetic Shared value is binding evidence.
The confirmed allocation index in `handlers/migration_authority_employee.rs`
already pins a tenant and verifies a root receipt. Reconcile that index with the
root lifecycle receipt and the actual binding owner; do not introduce a competing
mutable ACL or reinterpret old ambiguous rows as Personal.

At admission and every use, compare employee ID, allocation/binding generation,
owner or tenant, principal generation and credential epoch, active membership,
account identity and generation, consent, policy revision and revocation state.
Personal requires the exact live owner and 1:1 lifecycle binding. Shared requires
current membership and the existing product role. Background work needs its
record-bound approved authority; absence of a human is never permission to use
personal credentials. Any permitted Personal-to-home connector access must use
the explicit product-home authorization method and existing product rules.

Selection, credential acquisition/refresh, outbound send, pagination and result
publication must carry the same selected account and live authority. Recheck
after waits and locks and immediately before the external effect. On revocation,
cancel queued work, retire leases, drop credentials and refuse later pages or
output. A request already accepted by a remote provider cannot be recalled;
report that uncertainty instead of promising transactional remote revocation.

## Work packages and estimates

Estimates are engineer-days for one engineer familiar with these modules,
including focused tests and review fixes. They exclude provider consent delays,
credentials supplied by customers and production rollout. Ranges include schema
and source-owner uncertainty; they are not delivery commitments.

| Package | Concrete changes and acceptance | Estimate |
| --- | --- | --- |
| A: lifecycle read model | Inspect `crates/duduclaw-isolation/src/capability.rs`, `crates/duduclaw-spawnd/src/` lifecycle receipt handlers, gateway `handlers/migration_authority_employee.rs` and `handlers/migration_authority_bindings.rs`. Add a read-only trusted binding snapshot that returns immutable employee, Personal owner/Shared tenant, generation and tombstone. Define an explicit migration for missing old evidence, with no auto-grant. Test delete/recreate, owner reassignment, duplicate Personal binding and unreadable stores. | 3–5 days |
| B: production binding authority | Replace Shared synthesis in gateway `isolation_mcp/boot.rs`; use the snapshot in `isolation_mcp/authority.rs` identity, prepare, recheck, check_epoch and `BindingAuthority`. Keep `isolation_background.rs` receipts bound to original authority. Test Personal owner/non-owner, Shared membership removal, generation changes between checks, and restart. | 3–5 days |
| C: account lifecycle bridge | Wire trusted provision/revoke updates to `isolation_connectors.rs` catalog and `isolation_resources.rs` selection. Implement production `authorize_connector` and explicit product-home checks against lifecycle/account owners, with generation-bound selection receipts. Do not scan credentials to create catalog grants. Test cross-tenant and same-name account replacement, selection-to-send revoke, expiry and unavailable stores. | 3–5 days |
| D: non-Google providers | Supply native credential owner adapters and all selection/execution pairs through `isolation_connector_boot.rs`, `isolation_connector_registry.rs` and gateway `isolation_connector_{clients,odoo,database,channel,model,computer,search}.rs`; retain CLI executors in `mcp_isolation_connector_{odoo,channel}.rs`. Exercise Odoo/database policy, channel audience, secret redaction, bounded output and provider redirects. | 4–7 days |
| E: Google owner paths | Connect the authenticated requester to `google_identity/isolation_bridge.rs`, `isolation_connector_clients.rs` and Google identity source owners. Finish home/personal OAuth refresh and consent revoke plus delegated, service-account and Apps Script paths actually supported by the product. Unsupported sources must remain explicit failures. Test source replacement, wrong owner and revoke during refresh; use dedicated provider test accounts. | 5–9 days |
| F: boot and user experience | Populate real `BootSources` in gateway `isolation_mcp/boot.rs` and CLI `mcp_isolation_boot.rs` without replacing per-employee identity with a singleton registry. Check all required producers before admitting a protected run; wire structured reasons through `isolation_readiness.rs`, readiness RPC and the dashboard isolation status panel. Add localized actionable refusal/notice text with no credential or tenant details. | 2–4 days |
| G: end-to-end acceptance | Extend `mcp_isolation_parity_tests.rs` with production issuer output and actual boot composition; add native Linux owner/tenant/generation/revoke race and restart tests, then real-provider smoke tests for each installed family. Capture exact revisions, commands and sanitized receipts. Run independent security review and resolve findings before rollout. | 4–7 days |

Total: **24–42 engineer-days** (roughly 5–9 working weeks for one engineer), plus
external access and consent lead time. A → B/C → D/E → F → G is the dependency
order; D and E can overlap once the lifecycle contract is reviewed. A smaller
first provider release requires an explicit product scope decision; silently
dropping required tools is not full parity.

## Safe interim user-facing behavior

Keep protected startup fail-closed when required binding evidence or connector
producers are absent. Keep ordinary installations on their existing explicitly
selected mode; never retry a refused protected action through ambient host
credentials or an unprotected subprocess. Do not invite the employee to paste a
token into a prompt or grant EmployeeScope as a connector workaround.

The proposed dashboard notice should state: “Protected execution is not ready:
employee account bindings and connector services still need setup. No protected
connector request has been sent.” A Personal-specific refusal should say:
“This employee's personal account binding is not supported in protected mode
yet. Ask an administrator to check isolation readiness.” Distinguish unsupported
setup, revoked access and temporary service unavailability, with an administrator
status link and a sanitized correlation ID. These messages and notice are planned
work in package F, not UI changes delivered by this document.

Completion requires both positive and negative evidence: correct owner's calls
succeed, other owners/tenants fail, stale generations and revoked consent fail at
use time, and missing providers block admission. A passing catalog/list fixture
or a reachable network endpoint does not satisfy that gate.
