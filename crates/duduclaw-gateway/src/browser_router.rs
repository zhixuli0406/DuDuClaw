//! 5-layer auto-routing for browser automation.
//!
//! Decides which tier (L1–L5) should handle a given web request based on
//! the request's requirements and the agent's [`BrowserConfig`].

use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::{info, warn};

/// The five escalation layers for browser automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum BrowserTier {
    ApiFetch,        // L1 — reqwest / WebFetch
    StaticScrape,    // L2 — CSS / XPath selector
    HeadlessBrowser, // L3 — Playwright MCP
    SandboxBrowser,  // L4 — Container + Playwright
    ComputerUse,     // L5 — Virtual display + Claude vision
}

impl fmt::Display for BrowserTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiFetch => write!(f, "L1-ApiFetch"),
            Self::StaticScrape => write!(f, "L2-StaticScrape"),
            Self::HeadlessBrowser => write!(f, "L3-HeadlessBrowser"),
            Self::SandboxBrowser => write!(f, "L4-SandboxBrowser"),
            Self::ComputerUse => write!(f, "L5-ComputerUse"),
        }
    }
}

fn default_true() -> bool { true }
fn default_max_tier() -> BrowserTier { BrowserTier::StaticScrape }
fn default_max_pages() -> u32 { 20 }
fn default_max_minutes() -> u32 { 10 }

/// Fine-grained browser automation restrictions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserRestrictions {
    #[serde(default)]
    pub allow_form_submit: bool,
    #[serde(default)]
    pub allow_file_download: bool,
    #[serde(default = "default_max_pages")]
    pub max_pages_per_session: u32,
    #[serde(default = "default_max_minutes")]
    pub max_session_minutes: u32,
    #[serde(default)]
    pub screenshot_audit: bool,
    #[serde(default)]
    pub require_human_approval_for: Vec<String>,
}

impl Default for BrowserRestrictions {
    fn default() -> Self {
        Self {
            allow_form_submit: false,
            allow_file_download: false,
            max_pages_per_session: default_max_pages(),
            max_session_minutes: default_max_minutes(),
            screenshot_audit: false,
            require_human_approval_for: Vec::new(),
        }
    }
}

/// Top-level browser configuration (parsed from `CONTRACT.toml [browser]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_max_tier")]
    pub max_tier: BrowserTier,
    #[serde(default)]
    pub trusted_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default)]
    pub restrictions: BrowserRestrictions,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tier: default_max_tier(),
            trusted_domains: Vec::new(),
            blocked_domains: Vec::new(),
            restrictions: BrowserRestrictions::default(),
        }
    }
}

/// Describes what a browser request needs.
#[derive(Debug, Clone)]
pub struct BrowserRequest {
    pub url: String,
    pub needs_javascript: bool,
    pub needs_interaction: bool,
    pub needs_visual: bool,
}

/// The outcome of a successful routing decision.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub tier: BrowserTier,
    pub domain_trusted: bool,
    pub requires_human_approval: bool,
    pub reason: String,
}

/// Routing failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingError {
    BrowserDisabled,
    DomainBlocked(String),
    TierExceeded { required: BrowserTier, max: BrowserTier },
}

impl fmt::Display for RoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BrowserDisabled => write!(f, "browser automation is disabled"),
            Self::DomainBlocked(d) => write!(f, "domain is blocked: {d}"),
            Self::TierExceeded { required, max } => {
                write!(f, "required tier {required} exceeds max allowed {max}")
            }
        }
    }
}

impl std::error::Error for RoutingError {}

/// Routes browser requests to the appropriate automation tier.
pub struct BrowserRouter {
    config: BrowserConfig,
}

impl BrowserRouter {
    pub fn new(config: BrowserConfig) -> Self {
        Self { config }
    }

    /// Create a router from an optional `BrowserConfig` (e.g. extracted from
    /// the `[browser]` section of a parsed CONTRACT.toml). Uses defaults when `None`.
    pub fn from_contract(browser: Option<BrowserConfig>) -> Self {
        Self { config: browser.unwrap_or_default() }
    }

    /// Route a request to the appropriate tier.
    pub fn route(&self, request: &BrowserRequest) -> Result<RoutingDecision, RoutingError> {
        if !self.config.enabled {
            warn!("browser automation disabled — rejecting request to {}", request.url);
            return Err(RoutingError::BrowserDisabled);
        }

        let domain = extract_domain(&request.url);

        if domain_matches_any(&domain, &self.config.blocked_domains) {
            warn!(domain = %domain, "domain is blocked");
            return Err(RoutingError::DomainBlocked(domain));
        }

        let domain_trusted = domain_matches_any(&domain, &self.config.trusted_domains);

        // Determine minimum tier from request capabilities.
        let (mut tier, reason) = if request.needs_visual {
            (BrowserTier::ComputerUse, "visual verification requires ComputerUse (L5)")
        } else if request.needs_interaction {
            (BrowserTier::HeadlessBrowser, "interaction requires HeadlessBrowser (L3+)")
        } else if request.needs_javascript {
            (BrowserTier::HeadlessBrowser, "JavaScript requires HeadlessBrowser (L3+)")
        } else {
            (BrowserTier::ApiFetch, "static content — ApiFetch (L1) sufficient")
        };

        // Untrusted domains at L3 get escalated to L4 (sandbox).
        if !domain_trusted
            && tier >= BrowserTier::HeadlessBrowser
            && tier < BrowserTier::SandboxBrowser
        {
            info!(domain = %domain, "untrusted domain — escalating to SandboxBrowser (L4)");
            tier = BrowserTier::SandboxBrowser;
        }

        if tier > self.config.max_tier {
            warn!(required = %tier, max = %self.config.max_tier, "tier exceeds maximum");
            return Err(RoutingError::TierExceeded {
                required: tier,
                max: self.config.max_tier,
            });
        }

        let requires_human_approval =
            !self.config.restrictions.require_human_approval_for.is_empty()
                && self.check_request_needs_approval(request);

        info!(url = %request.url, tier = %tier, domain_trusted, requires_human_approval, reason, "routed");

        Ok(RoutingDecision {
            tier,
            domain_trusted,
            requires_human_approval,
            reason: reason.to_string(),
        })
    }

    /// Check whether a domain is allowed (i.e. not in the blocked list).
    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        !domain_matches_any(domain, &self.config.blocked_domains)
    }

    /// Check whether an action (e.g. `"form_submit"`, `"login"`) is freely
    /// allowed — returns `false` if it requires human approval.
    pub fn check_action_allowed(&self, action: &str) -> bool {
        !self.config.restrictions.require_human_approval_for.iter().any(|a| a == action)
    }

    fn check_request_needs_approval(&self, request: &BrowserRequest) -> bool {
        let actions = &self.config.restrictions.require_human_approval_for;
        (request.needs_interaction && actions.iter().any(|a| a == "interaction"))
            || (request.needs_visual && actions.iter().any(|a| a == "visual"))
    }
}

// -- Helpers ----------------------------------------------------------------

fn extract_domain(url: &str) -> String {
    let without_scheme = url.find("://").map(|i| &url[i + 3..]).unwrap_or(url);
    without_scheme
        .split(&['/', '?', '#', ':'][..])
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

fn domain_matches_any(domain: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| domain_matches(domain, p))
}

/// Supports simple glob: `*.example.com` matches `sub.example.com` but not `example.com`.
fn domain_matches(domain: &str, pattern: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        domain.ends_with(suffix)
            && domain.len() > suffix.len()
            && domain.as_bytes()[domain.len() - suffix.len() - 1] == b'.'
    } else {
        domain == pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str) -> BrowserRequest {
        BrowserRequest { url: url.into(), needs_javascript: false, needs_interaction: false, needs_visual: false }
    }

    #[test]
    fn static_url_routes_to_l1() {
        let r = BrowserRouter::new(BrowserConfig::default());
        assert_eq!(r.route(&req("https://example.com/page")).unwrap().tier, BrowserTier::ApiFetch);
    }

    #[test]
    fn js_url_routes_to_l3_when_trusted() {
        let r = BrowserRouter::new(BrowserConfig {
            max_tier: BrowserTier::HeadlessBrowser,
            trusted_domains: vec!["example.com".into()],
            ..Default::default()
        });
        let mut q = req("https://example.com/app");
        q.needs_javascript = true;
        assert_eq!(r.route(&q).unwrap().tier, BrowserTier::HeadlessBrowser);
    }

    #[test]
    fn untrusted_domain_escalates_to_l4() {
        let r = BrowserRouter::new(BrowserConfig { max_tier: BrowserTier::SandboxBrowser, ..Default::default() });
        let mut q = req("https://untrusted.org/form");
        q.needs_interaction = true;
        let d = r.route(&q).unwrap();
        assert_eq!(d.tier, BrowserTier::SandboxBrowser);
        assert!(!d.domain_trusted);
    }

    #[test]
    fn visual_routes_to_l5() {
        // Default max_tier is StaticScrape — must set ComputerUse explicitly to allow L5.
        let r = BrowserRouter::new(BrowserConfig { max_tier: BrowserTier::ComputerUse, ..Default::default() });
        let mut q = req("https://example.com");
        q.needs_visual = true;
        assert_eq!(r.route(&q).unwrap().tier, BrowserTier::ComputerUse);
    }

    #[test]
    fn max_tier_enforcement() {
        let r = BrowserRouter::new(BrowserConfig { max_tier: BrowserTier::StaticScrape, ..Default::default() });
        let mut q = req("https://example.com");
        q.needs_javascript = true;
        let err = r.route(&q).unwrap_err();
        assert!(matches!(err, RoutingError::TierExceeded { required: BrowserTier::SandboxBrowser, max: BrowserTier::StaticScrape }));
    }

    #[test]
    fn disabled_browser() {
        let r = BrowserRouter::new(BrowserConfig { enabled: false, ..Default::default() });
        assert_eq!(r.route(&req("https://x.com")).unwrap_err(), RoutingError::BrowserDisabled);
    }

    #[test]
    fn blocked_domain() {
        let r = BrowserRouter::new(BrowserConfig { blocked_domains: vec!["evil.com".into()], ..Default::default() });
        assert!(matches!(r.route(&req("https://evil.com/x")).unwrap_err(), RoutingError::DomainBlocked(_)));
    }

    #[test]
    fn glob_domain_matching() {
        let pats = vec!["*.example.com".to_string()];
        assert!(domain_matches_any("sub.example.com", &pats));
        assert!(domain_matches_any("deep.sub.example.com", &pats));
        assert!(!domain_matches_any("example.com", &pats));
        assert!(!domain_matches_any("notexample.com", &pats));
    }

    #[test]
    fn is_domain_allowed_checks_blocked() {
        let r = BrowserRouter::new(BrowserConfig { blocked_domains: vec!["*.blocked.org".into()], ..Default::default() });
        assert!(!r.is_domain_allowed("sub.blocked.org"));
        assert!(r.is_domain_allowed("safe.org"));
    }

    #[test]
    fn check_action_allowed_with_approval_list() {
        let r = BrowserRouter::new(BrowserConfig {
            restrictions: BrowserRestrictions {
                require_human_approval_for: vec!["login".into(), "form_submit".into()],
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(!r.check_action_allowed("login"));
        assert!(!r.check_action_allowed("form_submit"));
        assert!(r.check_action_allowed("navigate"));
    }

    #[test]
    fn from_contract_none_uses_defaults() {
        let r = BrowserRouter::from_contract(None);
        assert!(r.config.enabled);
        assert_eq!(r.config.max_tier, BrowserTier::StaticScrape);
    }

    #[test]
    fn default_restrictions() {
        let r = BrowserRestrictions::default();
        assert!(!r.allow_form_submit);
        assert!(!r.allow_file_download);
        assert_eq!(r.max_pages_per_session, 20);
        assert_eq!(r.max_session_minutes, 10);
        assert!(r.require_human_approval_for.is_empty());
    }
}
