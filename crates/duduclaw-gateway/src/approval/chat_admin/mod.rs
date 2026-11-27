pub(crate) mod card;
pub(crate) mod config;
pub(crate) mod deliver;
pub(crate) mod ledger;
pub(crate) mod receipts;
pub(crate) mod press;
pub(crate) mod step_up;
pub(crate) mod sync;
pub(crate) mod tier;

pub(crate) use ledger::LedgerEntry;

/// Identity supplied by the authenticated decision service, never by tool arguments.
pub(crate) enum DecisionActor {
    Dashboard { user_id: String },
    ChatAdmin(LedgerEntry),
}

pub(crate) use config::{
    card_valid_until, effective, load, parse, ChatAdminConfig, ChatChannel, ConfigError,
    EnabledAck, StepMethod,
};
pub(crate) use tier::{chat_tier, kind_key, step_up_selectable, tier, Tier, STEP_UP_KIND_KEYS};
