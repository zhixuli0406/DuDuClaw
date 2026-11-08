# Native Linux isolation v1.81.0 work breakdown (Personal bindings + all connectors)

Status: plan, 2026-11-08. Base: `feat/iso-v181-20261108` @ 4600119c9. Source: planning pass over
`docs/architecture/native-linux-isolation-connectors-plan.md` and the code. [SEC] = needs independent
security review. Most code is `#[cfg(target_os="linux")]`: compile/test on Linux only.

## Decisions
- D-1 Personal employee creation: no path creates one today (`duduclaw-migration/src/import_manifest.rs:173`
  stages proposals only from import; importer maps legacy bindings to `shared:` at
  `duduclaw-import/src/authority.rs:207`; `binding_plan` accepts only `shared:` at
  `handlers/migration_authority_bindings.rs:17`; `agents_create.rs` has no allocation path).
  Proposed: admin-only `isolation.employee_propose` with `binding_kind` + `owner_id` through the existing
  approval → allocation flow (A4).
- D-2 One connector account store: `ConnectorCatalog` (`isolation_connectors.rs:14-55`) becomes the only
  production store with binding + generation; standalone `BindingStore` stays a test double.
- D-3 Google sources: bridge supports Personal + Home OAuth; delegate / service-account refused at
  `google_identity/isolation_bridge.rs:76-78`; Apps Script has no connector tool.
- D-4 29 non-connector tools without production selectors (`mcp_isolation_parity_tests.rs:97-105`:
  file_read, execute_program, web_extract, …) are outside A–G.

## Owner decisions (2026-11-08)
- D-1: Personal protected employee is requested by the staff member (own account page) and approved by an Admin through the existing approval → allocation flow. A4 becomes `me.isolation.request_personal_employee` (self, owner = caller) + admin approval; dashboard employee creation in protected mode goes through the same proposal path.
- D-2: ConnectorCatalog is the single production store.
- D-3: delegate and service-account Google sources are in scope (E4).
- D-4: the 29 non-connector tools are in scope; a separate breakdown (package H) is required.

## Tasks (owned files, contract, tests)
- A1 [SEC] root allocation receipt records binding: `duduclaw-migration/src/allocation.rs` (Receipt :17-20
  + `principal_binding: Option<EmployeeBinding>` serde default; Specification :25 + `binding`),
  `confirmation.rs` allowlist :95-108, `duduclaw-spawnd/src/allocation.rs` (journal schema 1|2, :31; same
  op different binding ⇒ InvalidRequest). Tests: allocation_receipt_schema2_pins_personal_owner,
  allocation_schema1_journal_reads_as_unbound_legacy, allocation_same_operation_binding_change_rejected,
  extend iso_root_e2e_confirmed_allocation_binding_restart_and_compensation (:155).
- A2 [SEC] gateway `EmployeeLifecycle` read model: new `isolation_lifecycle.rs`;
  `handlers/migration_authority_employee.rs` (`resolve_confirmed_employee` :71-83 compares binding;
  `validate_current` :14; Personal owner must be Active, logged in, no forced password change like
  `migration_authority_bindings.rs:18-19`). Legacy None ⇒ Shared(pinned tenant) only. Errors
  Unavailable/Denied/Stale/Tombstoned. Tests: lifecycle_personal_owner_from_root_receipt,
  lifecycle_legacy_receipt_is_shared_only, lifecycle_delete_recreate_new_employee_old_tombstoned,
  lifecycle_owner_reassignment_is_stale, lifecycle_unreadable_store_unavailable,
  employee_proposal_personal_owner_requires_active_logged_in_user.
- A3 [SEC] users.db Personal exclusivity: `duduclaw-auth/src/db.rs` (`bind_agent_in_transaction` :1358),
  `duduclaw-auth/src/isolation.rs` (snapshot :54-60 + personal_owner, agent_member_count, same tx :67-88);
  mirror table `isolation_personal_agents`. Tests: personal_agent_refuses_second_binding,
  personal_agent_snapshot_reports_owner_and_member_count, personal_mirror_insert_is_immutable.
- A4 [SEC] `isolation.employee_propose` RPC: new `handlers/isolation_employee_rpc.rs`, register in
  `handlers/mod.rs` (~:567); admin + step-up; never auto-applies.
- B1 [SEC] `isolation_mcp/authority.rs` Personal support (identity :79-109, check_epoch :163-179,
  PreparedAuthority :33-44 + lifecycle, recheck :147-159, snapshot :256-270, BindingAuthority::validate
  :243-248). Personal: Human user == owner, mirror owner == owner, member count 1, never Background.
- B2 [SEC] `isolation_mcp/boot.rs` issuer (`record_authorization` :288-312, `human_authorization` :318-357):
  replace hard-coded Shared at :353 with lifecycle binding; non-owner ⇒ Revoked; re-read after I/O.
- B3 [SEC] `isolation_background.rs` RecordAuthority :29-50 (+binding, allocation), capture :107-116,
  recheck_receipt :195-225, `isolation_background/store.rs`; old receipts need re-approval (CHANGELOG).
- C1 [SEC] production `ConnectorCatalog` (`isolation-connectors.db` 0600; routes table account PK,
  provider, kind, binding_id, generation, active; opaque never-reused ids; generation in
  SelectedConnector, `recheck_binding` :199-214).
- C2 [SEC] real `authorize_connector` + product-home check (authority.rs impl BindingAuthority :242-255;
  boot.rs:166 constructor).
- C3 [SEC] new `isolation_connector_lifecycle.rs` + `handlers/isolation_connectors_rpc.rs`
  (`isolation.connector_list/provision/revoke`, admin step-up; revoke deactivates route before vault drop
  `isolation_connector_clients.rs:55`).
- C4 [SEC] product credential events update catalog (`google_personal/lifecycle.rs`, `owner_change.rs`,
  home OAuth connect/disconnect in `mcp_oauth`).
- C5 [SEC] migration `ConfirmationCategory::Connector` (`handlers/migration_authority.rs` :335-345, :361,
  refused today :638-645) via new `migration_authority_connectors.rs`.
- D1 [SEC] `ConnectorProviderFactory` trait in `isolation_connector_boot.rs`; `assemble` (:48-67) takes
  `Arc<dyn BindingAuthority>`; accounts only from the employee's active catalog routes.
- D2 [SEC] new `isolation_connector_native_home.rs`: Notion/GitHub home credential providers
  (`notion_workspace.rs:159`, `github_workspace.rs:272`) + selection policies (SharedHome only).
- D3 CLI `mcp_isolation_connector_factory.rs` wrapping Odoo/Channel executors; install in
  `duduclaw-cli/src/lib.rs` :9210-9212 before `start_gateway`.
- D4 [SEC] policy tests database/channel/search/model/computer/odoo.
- E1 [SEC] bridge gets real requester (`isolation_bridge.rs` new :37, source :52-84 replacing Unknown at
  :63/:72).
- E2 [SEC] new `google_identity/isolation_routes.rs` (routes from catalog; personal map = lifecycle owner).
- E3 [SEC] refresh / consent revoke recheck (`home_source/oauth_refresh_io.rs`, `google_personal/oauth.rs`).
- E4 [SEC] delegate / service-account sources (arm :76-78).
- F1 [SEC] `BootSources` → `connector_factory` + `catalog` (boot.rs :43-59, :129-146, :150-200,
  :255-268, :367-400, :439-493); `install_connector_factory()` like :79.
- F2 CLI `mcp_isolation_boot.rs` (:19-43, :68-84, :95-103) per-employee registry.
- F3 `isolation_readiness.rs` reasons (binding_evidence_missing, connector_setup_required{provider},
  personal_binding_conflict, revoked, service_unavailable) + `handlers/system_status.rs:93`.
- F4 `web/src/components/IsolationReadinessBanner.tsx`, `web/src/lib/api.ts:348`, i18n ×3.
- G1 [SEC] `mcp_isolation_parity_tests.rs` real issuer + real factory (62 tools).
- G2 [SEC] `crates/duduclaw-gateway/tests/iso_personal_chain.rs` (`#[ignore]` iso_root_e2e_*).
- G3 `scripts/live-test/linux-protected/connectors-acceptance.sh` + fixtures (old acceptance.sh targets
  the removed container_root_key backend).
- G4 docs status columns, plan doc, CHANGELOG.

## Waves (≤4 parallel worktrees; same-file edits serialized: authority.rs B1→C2, boot.rs B2→C2→F1)
| Wave | 1 | 2 | 3 | 4 |
|---|---|---|---|---|
| 1 | A1 | A3 | C1 | D2 |
| 2 | A2 | E1 | D4 | F4 |
| SR1 | A1 A2 A3 C1 | | | |
| 3 | B1 | B3 | C3 | D1 |
| 4 | B2 | A4 | E2 | D3 |
| SR2/SR3 | B1 B2 B3 A4 | C3 D1 D2 D4 | | |
| 5 | C2 | C4 | E3 | G3 |
| 6 | F1 | C5 | E4 | — |
| SR4 | C2 C4 C5 E1–E4 | | | |
| 7 | F2 | F3 | G2 | — |
| 8 | G1 | G4 | — | — |
| SR5 | F1–F3 G1 G2 + final whole-system review | | | |

## Infrastructure and acceptance
- Native root on Ubuntu 22.04 (192.168.0.45), 24.04, Debian 12:
  `DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1`;
  `DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-gateway --test iso_personal_chain -- --ignored iso_root_e2e_ --test-threads=1 --nocapture`.
- Provider test accounts: Google Workspace test tenant (2 users Personal, Home, delegate/SA), Notion,
  GitHub org/repo, disposable Odoo, Postgres/MySQL read-only user, Discord/LINE/Telegram bots, search key.
- Matrix (fixture alice owner, bob; employees alice-personal = Personal(alice), team = Shared):
  1 owner gmail_search ok†; 2 bob binding refused / forced ⇒ personal_binding_conflict†; 3 team Personal
  tool only via explicit home fallback; 4 member removed mid-run ⇒ next call refused ≤1 s†; 5 tenant uuid
  replaced ⇒ all refused; 6 delete/recreate same label ⇒ old capability + receipts refused†; 7 revoke
  between selection and send ⇒ no request, uncertain; 8 revoke during Google refresh ⇒ uncertain, next page
  refused; 9 revoke between Notion pages; 10 gateway+spawnd restart ⇒ old capability refused†; 11 same-name
  relink ⇒ new id; 12 missing factory/provider ⇒ connector_setup_required†; 13 background after password
  reset ⇒ refused; 14 Odoo forbidden / DB non-SELECT / unbound channel recipient refused before credential;
  15 live smoke per provider family. († also on 24.04 and Debian 12.)

## Plan-doc corrections
Package A files are `duduclaw-migration/src/allocation.rs` + spawnd allocation (not capability.rs); tenant
pin lives only in gateway `apply.db`; no Personal creation path exists; "principal generation" is the global
`acl_generation` (`duduclaw-auth/src/isolation.rs:75`); `install_boot_sources` has no caller and
BootSources holds per-employee providers as singletons (API change needed); Odoo/channel executors exist
only in the CLI crate; no production catalog file; Notion/GitHub are home-wide only; RecordAuthority has
`deny_unknown_fields`; old acceptance.sh is obsolete; 29 non-connector production-selector gaps unlisted.

## Estimate
Coding 4–5 working days (2 waves/day); security reviews +3–5; infrastructure acceptance 3–5.
Optimistic 10, expected 14, pessimistic 20 working days.

## Follow-ups found during implementation
- From D2: `mcp_oauth::refresh_token` (`mcp_oauth.rs:673`) uses `reqwest::Client::new()` which follows redirects; build it with `redirect(Policy::none())` and make `get_client_config` distinguish read errors from absence (owner: C4/E3, whichever edits `mcp_oauth` first). Notion has no enable flag: missing flag = allowed, explicit `[integrations] notion = false` = kill switch.
- From A1: `Specification.binding` is `Option<EmployeeBinding>` (legacy proposals have none); Shared carries `tenant_id` from the proposal — A2 must check it equals the tenant pinned in `apply.db`. Import validator now rejects Employee proposals with binding fields.
- From A3: `remove_personal_agent_in_transaction` also deletes all bindings for the agent and bumps the ACL generation. Triggers make two existing paths fail for Personal agents: offboarding with transfer (`org_write.rs` `move_bindings`) and `create_member_at_epoch` binding to a Personal agent. A2/A4 must define offboarding of a Personal owner (disable/tombstone the employee through the lifecycle path, then remove the mirror) and give these paths a clear refusal message. Mirror has no FK to `users`.
- From A2: `crate::isolation_lifecycle` is the read model (`read_lifecycle`, `recheck_with`; `resolve_confirmed_employee` delegates to it). The Personal mirror is written by the Employee apply path (`save_confirmed_receipt`) in its own users.db transaction, committed while the apply.db writer lock is held and before the confirmed row; a mirror failure refuses the step. `tombstone_personal_employee` (admin handover) marks the apply.db row disabled and then removes the mirror. Open: (1) no root action disables an Applied allocation, so the tombstone is gateway-side only; spawnd keeps the UID/storage and refuses a new live allocation with the same label until it gets one (owner: spawnd/A1). (2) Applying a Personal employee records the mirror but does not bind the owner in `user_agent_bindings`; B1 requires member count 1, so A4 must bind the owner. (3) The tombstone has no RPC yet: A4 adds it with admin + step-up. (4) Offboarding a Personal owner without a transfer is still allowed; the lifecycle then reads Denied (owner not Active) until an admin tombstones it. (5) `readback_shared_employee` now refuses Personal employees, so the UserAgentBinding/DataGrant/Schedule/Channel migration categories never target one.
- From C3: F1 must call `isolation_connector_lifecycle::install_credential_material(vault)` with the same `AccountCredentialVault` the executors use; without it revoke still fences the route but drops no in-process material. Admin step-up reuses the migration dashboard proof (`UserDb::migration_step_up`; `isolation.connector_step_up` is a fourth method writing the same proof). Shared routes always bind the users.db installation tenant (`duduclaw_auth::isolation::read_isolation_tenant`); Personal owners must be Active, logged in, no forced password change. No dashboard page yet (F4 or follow-up).
- From D1: account lists come from `plan_connector_accounts` (own Personal/SharedTenant routes + SharedHome routes of the factory's `home_binding` for Personal employees and same-tenant Shared employees). Database/computer/channel routes also need per-account provisioning details (`ConnectorAccountDetails`: source name / website credential id / channel route + token); no production store exists yet — C3 must write one alongside `isolation.connector_provision`, and F1 must supply it plus `home_binding` (tenant from `apply.db`). The CLI Odoo/channel executors are bound per employee provider, so D3 supplies `OdooExecutorFactory`/`ChannelExecutorFactory` (with declared tool coverage), not fixed executors. A provider with no route is registered as a refusing adapter (`Unavailable`); F3 can surface it as `connector_setup_required`. `InstalledConnectorProviders` does not pin the binding (selection still checks it per call); F1 should rebuild on binding change.
- From the post-merge fix (5c485cf1a+): duduclaw-import's pinned `session/tasks/cron-v1793.sql` catalogs now describe v1.80 stores, so a v1.79.3 home cannot import those three stores. Required before release: accept a set of known schema versions (v1.79.3 and v1.80.x), each with its own pinned catalog and column mapping; A2A-owned goals stay excluded; cron actor columns never read. (Task M1, owner: migration.)
- From B1: `GatewayAuthority` takes the employee principal only from `isolation_lifecycle` (fresh `read_lifecycle_with` at prepare, `recheck_with` against the lifecycle pinned in `PreparedAuthority` on every recheck; Unavailable ⇒ `Unavailable`, Denied/Stale/Tombstoned ⇒ `Revoked`). The grant binding must equal the lifecycle binding (else `BindingMismatch`), so until B2 replaces the hard-coded Shared at `boot.rs:353` every Personal-employee run is refused at prepare. B2 must issue `lifecycle.binding` and apply `authority::personal_facts` (now `pub(super)`) to its ingress snapshot before and after I/O. Personal contract (exact comparisons, also on every `check_epoch` users.db re-read): `Principal::Human` user == lifecycle owner == snapshot human, `personal_owner == Some(owner)`, `agent_member_count == 1` and the owner has a direct binding; `Principal::Background` is refused before any I/O. A4 must bind the owner on apply (unbound owner ⇒ refused). A non-admin principal without access still fails in `read_isolation_authority` and surfaces as `Unavailable` from `identity` (pre-existing). Shared is unchanged except that `check_epoch` now also refuses when a Personal mirror appears under the label. C2: `authorize_connector` should start from `self.identity(.., None)` (it enforces all of the above) before the catalog route check. Background records keep running as `Human(owner)` via `record_authorization`, so B3 needs no Background principal for Personal employees. Tests use an injected root (`GatewayAuthority.root`, production `RootAllocator`); F1 needs no change.
- New task C6 [SEC] (from D1): a per-account connector settings store for database source names, website/computer-login credential ids and channel routes, written by C3 provision (admin) and read by `ConnectorAccountDetails`; F1 passes it with `home_binding` (tenant pinned in apply.db). Owner-facing admin page for isolation connectors (C3 RPCs incl. `isolation.connector_step_up`) is a new surface: flow → design canvas → implementation (task F5).
