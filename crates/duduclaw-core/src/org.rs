//! WP-ORG — organisational placement maps for expert packs and team staging.
//!
//! Expert packs and premium team templates place their agents into the
//! existing org primitives (`[agent] department` + `reports_to`). This module
//! is the single source of the *default* placements so the CLI converter
//! (`expert convert-teams`), the CLI installer (`expert install`), and the
//! gateway team-staging path can never drift:
//!
//! - `department_for_kit` — which functional department a shared worker kit
//!   (`teams/_departments/<kit>/`) belongs to. Department names are zh-TW
//!   (the product's primary locale; `is_valid_department` allows CJK) and are
//!   plain data, not identifiers.
//! - `industry_category` — which catalog category an industry team pack is
//!   listed under on the dashboard.
//! - `rank_for_role` — the display rank (階級) derived from the canonical
//!   role string. Rank is intentionally **derived, never stored**: the org
//!   tree (`reports_to`) is the authority, rank only labels it.

/// Display rank derived from an agent's role string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgRank {
    /// Cross-team decision layer (CEO 幕僚長 kit).
    Executive,
    /// Team lead / front desk — a valid `--attach-under` target.
    Manager,
    /// Department specialist / worker.
    Staff,
}

impl OrgRank {
    /// Stable wire string (`executive` / `manager` / `staff`).
    pub fn as_str(&self) -> &'static str {
        match self {
            OrgRank::Executive => "executive",
            OrgRank::Manager => "manager",
            OrgRank::Staff => "staff",
        }
    }

    /// Parse a wire string back (unknown ⇒ `None`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "executive" => Some(OrgRank::Executive),
            "manager" => Some(OrgRank::Manager),
            "staff" => Some(OrgRank::Staff),
            _ => None,
        }
    }
}

/// Derive a display rank from a role string (canonical kebab-case per
/// `AgentRole::as_str`, plus the expert-pack `front_desk` roster name;
/// separators are normalised so `team_leader` and `team-leader` agree).
/// Unknown roles rank as staff — the safest floor for an attach-target
/// allowlist.
pub fn rank_for_role(role: &str) -> OrgRank {
    let normalised: String = role
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == '_' || c == ' ' { '-' } else { c })
        .collect();
    match normalised.as_str() {
        "ceo" => OrgRank::Executive,
        "main" | "front-desk" | "team-leader" | "product-manager" => OrgRank::Manager,
        _ => OrgRank::Staff,
    }
}

/// Default functional department (zh-TW) for a shared worker kit under
/// `teams/_departments/<kit>/`. Unknown kits get no department — the agent
/// simply stays department-less, exactly the pre-WP-ORG behaviour.
pub fn department_for_kit(kit: &str) -> Option<&'static str> {
    match kit.trim() {
        "docs-admin" => Some("行政"),
        "billing-admin" => Some("財務"),
        "care-followup" => Some("客服"),
        "dispatch-scheduler" => Some("營運"),
        "inventory" => Some("倉管"),
        "marketing" => Some("行銷"),
        "sales-followup" => Some("業務"),
        _ => None,
    }
}

/// Catalog category slug for an industry team pack. Slugs are stable machine
/// keys (the dashboard localises the labels); unknown industries fall into
/// `other` so a future 23rd pack still lists.
pub fn industry_category(industry: &str) -> &'static str {
    match industry.trim() {
        "clinic" | "tcm" | "pharmacy" | "vet" | "homecare" => "health",
        "accounting" | "lawfirm" | "insurance" | "hr" | "b2bsales" => "professional",
        "ecommerce" | "usedcar" | "realestate" => "retail",
        "fitness" | "wedding" | "funeral" | "moving" | "interior" | "autorepair"
        | "hospitality" => "lifestyle",
        "education" | "childcare" => "education",
        _ => "other",
    }
}

/// All catalog category slugs in display order (dashboard section order).
pub const CATALOG_CATEGORIES: [&str; 6] =
    ["health", "professional", "retail", "lifestyle", "education", "other"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_round_trip_and_derivation() {
        assert_eq!(rank_for_role("main"), OrgRank::Manager);
        assert_eq!(rank_for_role("front_desk"), OrgRank::Manager);
        assert_eq!(rank_for_role("team_leader"), OrgRank::Manager);
        assert_eq!(rank_for_role("ceo"), OrgRank::Executive);
        assert_eq!(rank_for_role("worker"), OrgRank::Staff);
        assert_eq!(rank_for_role("specialist"), OrgRank::Staff);
        assert_eq!(rank_for_role("no-such-role"), OrgRank::Staff);
        for r in [OrgRank::Executive, OrgRank::Manager, OrgRank::Staff] {
            assert_eq!(OrgRank::parse(r.as_str()), Some(r));
        }
        assert_eq!(OrgRank::parse("boss"), None);
    }

    #[test]
    fn kit_departments_are_valid_names() {
        for kit in [
            "docs-admin",
            "billing-admin",
            "care-followup",
            "dispatch-scheduler",
            "inventory",
            "marketing",
            "sales-followup",
        ] {
            let dept = department_for_kit(kit).expect(kit);
            assert!(crate::is_valid_department(dept), "{dept}");
        }
        assert_eq!(department_for_kit("no-such-kit"), None);
        assert_eq!(department_for_kit(" docs-admin "), Some("行政"));
    }

    #[test]
    fn all_22_industries_have_a_category() {
        let industries = [
            "accounting",
            "autorepair",
            "b2bsales",
            "childcare",
            "clinic",
            "ecommerce",
            "education",
            "fitness",
            "funeral",
            "homecare",
            "hospitality",
            "hr",
            "insurance",
            "interior",
            "lawfirm",
            "moving",
            "pharmacy",
            "realestate",
            "tcm",
            "usedcar",
            "vet",
            "wedding",
        ];
        for i in industries {
            let cat = industry_category(i);
            assert_ne!(cat, "other", "{i} should be categorised");
            assert!(CATALOG_CATEGORIES.contains(&cat));
        }
        assert_eq!(industry_category("florist"), "other");
    }
}
