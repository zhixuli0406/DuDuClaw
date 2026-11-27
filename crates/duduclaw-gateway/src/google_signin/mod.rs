//! Company Google sign-in primitives. HTTP flows are registered separately.
pub mod oidc;
pub mod config;

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod oidc_tests;
