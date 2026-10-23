//! Read-only views of other people's conversations with an AI employee
//! (B5 manager view, later B5b for direct Viewer bindings).
//!
//! Classification uses stored host-owned origins. The dedicated oversight
//! RPCs layer live reporting-line checks, activation windows, masking and
//! mandatory audit over a read-only SQL scope. Existing readers
//! (`chat.sessions.*`, `runs.*`) retain their direct-binding gates.

pub mod classify;
pub mod mask;
pub mod reader;
pub mod limits;
pub mod audit;
pub mod receipts;
