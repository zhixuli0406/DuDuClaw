//! Coverage contract only: this table does not participate in authorization.
//! Scoped rows describe the target wiring; blocked rows describe current behavior.

use super::{GrantAction, GrantGroup};
use GrantAction::*;
use GrantGroup::*;
use Coverage::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    Scoped(GrantAction),
    AdminOnly,
    NotDelegated(&'static str),
}

pub(crate) const METHOD_COVERAGE: &[(&str, GrantGroup, Coverage)] = &[
    ("cron.list", Schedules, Scoped(Read)),
    ("reminders.list", Schedules, Scoped(Read)),
    ("assign_schedules.list", Schedules, Scoped(Read)),
    ("cron.add", Schedules, Scoped(Create)),
    ("cron.update", Schedules, Scoped(Edit)),
    ("cron.pause", Schedules, Scoped(Toggle)),
    ("cron.resume", Schedules, Scoped(Toggle)),
    ("assign_schedules.pause", Schedules, Scoped(Toggle)),
    ("assign_schedules.resume", Schedules, Scoped(Toggle)),
    ("cron.remove", Schedules, Scoped(Remove)),
    ("reminders.cancel", Schedules, Scoped(Remove)),
    ("assign_schedules.remove", Schedules, Scoped(Remove)),
    ("cron.run_now", Schedules, Scoped(RunNow)),
    ("cron.templates", Schedules, NotDelegated("no agent data")),
    ("heartbeat.trigger", Schedules, NotDelegated("pending: binding check lands with the MGR-1 fix; grant alternative added after it merges")),
    ("heartbeat.status", Schedules, NotDelegated("company-wide summary, unchanged in v1.86")),
    // Schema migration pending: list/create/update/remove/history become Scoped
    // with the department-rule package; breakers and ticks remain AdminOnly.
    ("autopilot.list", Automation, AdminOnly),
    ("autopilot.create", Automation, AdminOnly),
    ("autopilot.update", Automation, AdminOnly),
    ("autopilot.remove", Automation, AdminOnly),
    ("autopilot.history", Automation, AdminOnly),
    ("autopilot.breakers", Automation, AdminOnly),
    ("ticks.sources", Automation, AdminOnly),
    ("ticks.recent", Automation, AdminOnly),
    ("skills.list", Skills, Scoped(Read)),
    ("skills.search", Skills, Scoped(Read)),
    ("skills.recent", Skills, Scoped(Read)),
    ("skills.content", Skills, Scoped(Read)),
    ("skills.archived", Skills, Scoped(Read)),
    ("skills.set_enabled", Skills, Scoped(Toggle)),
    ("skills.remove", Skills, Scoped(Remove)),
    ("skills.restore", Skills, Scoped(Restore)),
    ("skills.install_request", Skills, Scoped(RequestInstall)),
    ("skills.install", Skills, AdminOnly),
    ("skills.share", Skills, NotDelegated("writes new skill content; owner or admin only")),
    ("skills.adopt", Skills, NotDelegated("writes new skill content; owner or admin only")),
    ("skills.vet", Skills, NotDelegated("authenticated preview; no employee skill mutation")),
    ("skills.leaderboard", Skills, NotDelegated("existing callable-skill visibility; no department grant alternative")),
    ("skills.shared", Skills, NotDelegated("shared catalog, no department-scoped agent data")),
    ("skills.custom_create", Skills, NotDelegated("custom skill authoring uses existing creator authority")),
    ("skills.custom_generate", Skills, NotDelegated("custom skill generation uses existing creator authority")),
    ("skills.custom_update", Skills, NotDelegated("custom skill editing uses existing creator authority")),
    ("skills.custom_submit", Skills, NotDelegated("custom skill submission uses existing approval workflow")),
    ("skills.custom_list", Skills, NotDelegated("custom skill listing uses existing creator visibility")),
    ("skills.custom_retire", Skills, NotDelegated("custom skill retirement uses existing creator or admin authority")),
    ("install_requests.list", Skills, NotDelegated("existing approval queue visibility and signature chain")),
    ("install_requests.mine", Skills, NotDelegated("own install requests only; no department grant alternative")),
    ("install_requests.decide", Skills, NotDelegated("existing approval signature chain; grants do not authorize decisions")),
    ("cost.department_summary", Cost, Scoped(Read)),
    ("cost.department_recent", Cost, Scoped(Read)),
    ("cost.by_model", Cost, Scoped(Read)),
    ("cost.summary", Cost, AdminOnly),
    ("cost.agents", Cost, AdminOnly),
    ("cost.recent", Cost, AdminOnly),
    ("cost.by_role", Cost, AdminOnly),
    ("budget.incidents", Cost, NotDelegated("pending MGR-1 visibility filter")),
    ("billing.usage", Cost, NotDelegated("company-wide summary, unchanged in v1.86")),
    ("billing.history", Cost, NotDelegated("company-wide summary, unchanged in v1.86")),
    ("analytics.summary", Cost, NotDelegated("company-wide summary, unchanged in v1.86")),
    ("analytics.conversations", Cost, NotDelegated("company-wide summary, unchanged in v1.86")),
    ("analytics.cost_savings", Cost, NotDelegated("company-wide summary, unchanged in v1.86")),
    ("billing.plan", Cost, NotDelegated("billing unavailable in the current edition; existing refusal unchanged")),
];

pub(crate) fn coverage_for(method: &str) -> Option<(GrantGroup, Coverage)> {
    METHOD_COVERAGE.iter().find(|(name, _, _)| *name == method)
        .map(|(_, group, coverage)| (*group, *coverage))
}

#[cfg(test)]
#[path = "methods_tests.rs"]
mod tests;
