pub mod avatar_seats;
pub mod avatar;
#[cfg(test)]
mod avatar_signin_tests;
pub mod account_email_history;
pub mod acl;
pub mod admin_approval_rule;
pub mod admin_eligibility;
pub mod credential_changes;
pub mod chat_link;
pub mod db;
pub mod dept_grants;
pub mod jwt;
pub mod google_delegate_attestation;
pub mod google_identity;
pub mod google_signin;
pub mod signin_members;
pub mod signin_onboarding;
pub mod models;
pub mod org;
pub mod org_write;
pub mod team_history;
pub mod otp;
pub mod user_update;
pub mod user_invites;
pub mod members;
pub mod onboarding;
pub mod invitation_content;
mod user_row;
mod authority_migrations;
#[cfg(test)]
mod test_support;

pub use acl::UserContext;
pub use db::{busy_timeout_at_least, AclDeliveryError, UserDb, ACL_DELIVERY_WAIT};
pub use jwt::JwtConfig;
pub use models::*;
pub use org::{EffectiveAccess, GrantSource, OrgError, OverseenReport, TeamGrant, MAX_ORG_DEPTH};
pub use org_write::{OffboardOutcome, Reparent};
pub use otp::{ChannelIdentity, OtpChallenge, OtpError};
pub use user_update::{NewUser, UserUpdate, UserWriteError};

pub mod mfa;
pub mod migration;
pub mod isolation;
pub use isolation::{IsolationAuthoritySnapshot, read_isolation_authority, read_isolation_authority_in};

pub mod remembered_sessions;

pub mod status_tokens;
pub mod member_keys;

/// RM-134 (v1.82): hashed store for dashboard-issued MCP API keys.
pub mod mcp_keys;

pub mod odoo_credentials;
