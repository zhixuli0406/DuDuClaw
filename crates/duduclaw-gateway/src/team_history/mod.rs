//! Read-only views of other people's conversations with an AI employee
//! (B5 manager view, later B5b for direct Viewer bindings).
//!
//! This first part is the pure classification of one session from its stored
//! origin ([`crate::session_origin`]). Nothing calls it yet: the permission
//! check, masking, RPCs and audit come in later changes, and existing readers
//! (`chat.sessions.*`, `runs.*`) are unchanged.

pub mod classify;
