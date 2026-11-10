//! RM-173: classification of conversations archived before the
//! `sessions.archive_reason` column existed (v1.79.3, v1.80.x, and any
//! archived row left unlabelled by an interrupted migration or a downgrade).
//!
//! Before RM-173 two writers set `sessions.archived_at`:
//!
//! * `SessionManager::delete_session` — a user clear (`/new`, a channel reset,
//!   a deleted Discord thread). It archives a LIVE row only
//!   (`WHERE archived_at IS NULL`).
//! * the inactivity maintenance task — `cleanup_inactive(72)`, run at gateway
//!   boot and every 6 hours — archived every live row with
//!   `last_active < now - 72h`.
//!
//! So an inactivity archive always has `archived_at - last_active >= 72h`,
//! and a conversation idle that long is archived by maintenance before a
//! user can clear it (a clear of an archived row is a no-op), so a user clear
//! has a gap under 72 hours except for negligible edges (a gateway that was
//! down for days). Every doubtful row — gap under 72 hours, either timestamp
//! missing or unparseable — is classified a user clear: the privacy-safe side
//! (kept out of team history and of migration imports).
//!
//! The gateway (`SessionManager::migrate_legacy_user_clears`) and the
//! migration importer (`duduclaw-import`, sessions.db readers) share this one
//! function. The gateway applies it at EVERY start to every archived row
//! with no `archive_reason` and stores the result as a label; RM-173 writers
//! never archive without a reason (the importer's own archived sessions are
//! written `retention`), so only a pre-RM-173 store or an older binary run on
//! the store (a downgrade) leaves such rows. The importer reads a stored
//! label as is, and applies this function to a still-unlabelled row (a
//! v1.79.3 / v1.80.x store, or a v1.81 store written by an older binary
//! since its last gateway start). Both also treat a row carrying
//! `user_cleared_at` as a user clear. So on the same row they reach the same
//! verdict. What neither can see: a label that is already wrong in the
//! store — e.g. a conversation an older binary un-archived without dropping
//! its `retention` label, then archived in place on a user clear — and a
//! user clear made 72 hours or more after the last activity (see above).

use chrono::{DateTime, FixedOffset};

/// Inactivity threshold of the pre-RM-173 maintenance task
/// (`cleanup_inactive(72)`), in hours: a legacy archive at least this long
/// after the row's last activity is an inactivity (retention) archive.
pub const LEGACY_INACTIVITY_ARCHIVE_HOURS: i64 = 72;

fn parse(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

/// `true` iff a legacy archived conversation (archived with no recorded
/// reason) was archived by the inactivity maintenance task: both timestamps
/// are RFC 3339 (chrono's `to_rfc3339`, any offset, any sub-second
/// precision) and `archived_at - last_active` is at least
/// [`LEGACY_INACTIVITY_ARCHIVE_HOURS`]. Anything else — a shorter gap, an
/// archive before the last activity, an unparseable or empty timestamp — is
/// `false`: treat the row as a user clear.
pub fn legacy_archive_is_retention(archived_at: &str, last_active: &str) -> bool {
    let (Some(archived), Some(active)) = (parse(archived_at), parse(last_active)) else {
        return false;
    };
    archived.signed_duration_since(active)
        >= chrono::TimeDelta::hours(LEGACY_INACTIVITY_ARCHIVE_HOURS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVE: &str = "2026-11-01T00:00:00+00:00";

    #[test]
    fn exactly_72_hours_is_retention() {
        assert!(legacy_archive_is_retention("2026-11-04T00:00:00+00:00", ACTIVE));
    }

    #[test]
    fn just_under_72_hours_is_a_user_clear() {
        assert!(!legacy_archive_is_retention("2026-11-03T23:59:00+00:00", ACTIVE));
        assert!(!legacy_archive_is_retention(
            "2026-11-03T23:59:59.999999999+00:00",
            ACTIVE
        ));
    }

    #[test]
    fn long_gaps_are_retention() {
        assert!(legacy_archive_is_retention("2026-11-04T08:00:00+00:00", ACTIVE));
        assert!(legacy_archive_is_retention("2027-01-01T00:00:00Z", ACTIVE));
    }

    #[test]
    fn short_gaps_are_user_clears() {
        assert!(!legacy_archive_is_retention("2026-11-01T01:00:00+00:00", ACTIVE));
        assert!(!legacy_archive_is_retention(ACTIVE, ACTIVE));
    }

    #[test]
    fn archive_before_last_activity_is_a_user_clear() {
        assert!(!legacy_archive_is_retention("2026-10-01T00:00:00+00:00", ACTIVE));
    }

    #[test]
    fn unparseable_or_missing_timestamps_are_user_clears() {
        for bad in ["", "then", "now", "2026-11-10 00:00:00", "2026-11-10", "1762732800"] {
            assert!(!legacy_archive_is_retention(bad, ACTIVE), "archived_at {bad:?}");
            assert!(
                !legacy_archive_is_retention("2027-01-01T00:00:00+00:00", bad),
                "last_active {bad:?}"
            );
        }
    }

    #[test]
    fn offsets_are_compared_as_instants() {
        // 2026-11-04T08:00+08:00 == 2026-11-04T00:00Z: exactly 72h.
        assert!(legacy_archive_is_retention("2026-11-04T08:00:00+08:00", ACTIVE));
        // 2026-11-04T07:59+08:00 is 71h59m after ACTIVE.
        assert!(!legacy_archive_is_retention("2026-11-04T07:59:00+08:00", ACTIVE));
        // last_active given in -05:00: 2026-10-31T19:00-05:00 == ACTIVE.
        assert!(legacy_archive_is_retention(
            "2026-11-04T00:00:00Z",
            "2026-10-31T19:00:00-05:00"
        ));
    }

    #[test]
    fn nanosecond_precision_is_honoured() {
        assert!(legacy_archive_is_retention(
            "2026-11-04T04:00:00.123456789+00:00",
            "2026-11-01T04:00:00.123456789+00:00"
        ));
        assert!(!legacy_archive_is_retention(
            "2026-11-04T04:00:00.123456788+00:00",
            "2026-11-01T04:00:00.123456789+00:00"
        ));
    }
}
