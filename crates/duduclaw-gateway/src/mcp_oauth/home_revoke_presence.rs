//! Read-only Home grant presence for an already-held Personal revoke lease.
//! Private control/ticket interpretation remains owned by the H1 store.
use super::super::HomeRevokePresence;
use super::*;

pub(in crate::mcp_oauth) fn read(
    home: &Path,
    lease: &crate::google_personal::grants::Lease,
) -> Result<HomeRevokePresence> {
    if std::fs::canonicalize(home).ok().as_deref() != Some(lease.home()) {
        return Err(Error::Stale);
    }
    // The grant lease precedes these short source/config/client/token locks.
    // No lock or borrowed store state escapes this synchronous read.
    home_source::with_metadata(home, |g, source| {
        transaction(g, |rows| classify(rows, source, host_time_now(g)))
    })
}

pub(in crate::mcp_oauth) fn read_access_token(
    home: &Path,
    lease: &crate::google_personal::grants::Lease,
) -> Result<zeroize::Zeroizing<String>> {
    if std::fs::canonicalize(home).ok().as_deref() != Some(lease.home()) {
        return Err(Error::Stale);
    }
    home_source::with_metadata(home, |g, source| {
        transaction(g, |rows| {
            if classify(rows, source, host_time_now(g))? != HomeRevokePresence::GoogleCredential {
                return Err(Error::Invalid);
            }
            let stored = token_view(rows.google().ok_or(Error::Invalid)?)?;
            super::super::decode_secret(home, "google", &stored.access_token)
                .map(zeroize::Zeroizing::new)
                .ok_or(Error::Invalid)
        })
    })
}

fn classify(rows: &Rows, source: &ObservedSource, now: u64) -> Result<HomeRevokePresence> {
    // Generic/custom providers cannot establish a different Google account.
    if rows.rows.iter().any(|row| row["provider_id"] != "google") {
        return Ok(HomeRevokePresence::OtherProvider);
    }
    let Some(row) = rows.google() else {
        return Ok(HomeRevokePresence::NoCredential);
    };
    // Rows::read has already validated the typed control, credential/payload
    // correspondence and any present token DTO. Never migrate on this path.
    let control = rows.control()?;
    if let Some(control) = &control {
        control.ready()?;
        if let Some(ticket) = &control.active_connect {
            // A Pending ticket has not exchanged: our grant lease prevents
            // its exchange from starting until revoke has completed. An
            // Exchanging ticket may already have created an unknown grant.
            if ticket.phase != Phase::Pending
                || ticket.generation != control.source_generation
                || ticket.expires_at <= now
                || !ticket.matches_source(source.binding())
            {
                return Err(Error::Stale);
            }
        }
    }
    if row.get("access_token").is_some() {
        return Ok(HomeRevokePresence::GoogleCredential);
    }
    // An old Google row with no token is not an H1 tombstone. A valid typed
    // control alone also cannot bless partial/residual token fields.
    let control = control.ok_or(Error::Invalid)?;
    if control.credential.is_some()
        || TOKEN_FIELDS
            .iter()
            .filter(|&&field| field != "provider_id")
            .any(|&field| row.get(field).is_some())
    {
        return Err(Error::Invalid);
    }
    Ok(HomeRevokePresence::NoCredential)
}
