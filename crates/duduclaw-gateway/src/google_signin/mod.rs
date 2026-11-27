//! Company Google sign-in primitives. HTTP flows are registered separately.
pub mod oidc;
pub mod config;
pub(crate) mod routes;

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod oidc_tests;

pub(crate) mod avatar;

#[cfg(test)]
mod avatar_tests;

pub(crate) mod onboarding;

#[cfg(test)]
mod onboarding_tests;
