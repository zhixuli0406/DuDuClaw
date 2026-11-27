//! Gateway-only Google writes (RM-131). Tool programs and scope checks are
//! separate work packages; this module does not grant permission to write.

pub(crate) mod args;
pub(crate) mod args_docs;
pub(crate) mod program_docs;
pub(crate) mod program;
pub(crate) mod scope;
pub(crate) mod transport;

#[cfg(test)]
mod program_tests;
