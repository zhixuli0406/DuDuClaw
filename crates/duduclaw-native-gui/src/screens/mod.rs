pub mod about;
// S4b third wave — the "AI 員工" page (p11 list / p12 detail). `agents_data`
// (types + pure parsing), `agents_list` (p11 master list column),
// `agents_summary` (p11 right mini-summary), `agents_detail` (p12 full
// detail page) are all siblings of `agents`, split off for the same
// file-size reason `dashboard`/`dashboard_cards` are split (see `agents.rs`'s
// own doc comment).
pub mod agents;
mod agents_data;
mod agents_detail;
mod agents_list;
mod agents_summary;
pub mod chat;
pub mod console;
pub mod dashboard;
mod dashboard_cards;
pub mod gallery;
// S4b second wave — the "目標" page (p08). `goals_inspector` (right
// inspector) and `goals_data` (types + pure parsing/filtering) are both
// siblings of `goals`, split off for the same file-size reason
// `dashboard`/`dashboard_cards` are split (see `goals.rs`'s own doc
// comment).
pub mod goals;
mod goals_data;
mod goals_inspector;
pub mod inbox;
mod inbox_rows;
pub mod language_picker;
pub mod login;
pub mod prototypes;
pub mod shell;
// S4b third wave — the "任務" list page (p09) + its full detail page (p10).
// `tasks_data` (types + pure parsing/filtering), `tasks_quickview` (list
// page's right-column quick view), `tasks_detail` (the in-page full detail
// view `tasks_quickview` links to), and `tasks_detail_data` (that detail
// page's own data model + fetch/write orchestration, split out of
// `tasks_detail` to keep it under 800 lines too) are all siblings of
// `tasks`, same file-size-driven split `goals`/`goals_data`/
// `goals_inspector` establish — see `tasks.rs`'s own module doc comment.
pub mod tasks;
mod tasks_data;
mod tasks_detail;
mod tasks_detail_data;
mod tasks_quickview;
// Column 1/Column 2/shared-row internals of `shell.rs` — see that file's
// header comment for why the three-column app shell is split across
// multiple files (this crate's own <300-line-per-file convention). Not
// `pub`: nothing outside `screens` needs these directly, only `shell.rs`
// itself (`pub(super)` on each module's own entry point).
mod shell_content_list;
mod shell_row;
mod shell_sidebar;
