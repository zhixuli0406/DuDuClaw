//! Compatibility exports for computer-use callers of the general turn table.

pub use crate::turn_context::{
    MAX_TURN_ID_LEN, TurnGuard, decision_context_for, register, register_current_turn,
    reply_channel_for, valid_turn_id,
};
pub(crate) use crate::turn_context::{attended_live_for, target_for};
