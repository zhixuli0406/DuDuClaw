//! Native OS start intent, separate from all human/chat principal proofs.
mod proof;
use super::companion::Companion;
use std::{
    path::Path,
    sync::{Arc, Weak},
    time::Duration,
};

pub(crate) const ACTOR: &str = "本機原生使用者（平台身分未驗證）";
pub(crate) struct Permit {
    proof: proof::Permit,
    issuer: Weak<Companion>,
}
#[derive(Clone)]
pub(crate) struct Authorization {
    proof: proof::Authorization,
    issuer: Weak<Companion>,
}
pub(crate) struct Scope {
    _proof: proof::Scope,
}

fn binding(companion: &Arc<Companion>, home: &Path, agent: &str) -> Result<proof::Binding, String> {
    #[cfg(unix)]
    {
        if !super::policy::platform_supported()
            || !duduclaw_core::is_valid_agent_id(agent)
            || crate::ephemeral::is_ephemeral_id(agent)
        {
            return Err("native_start_unsupported".into());
        }
        let home = home
            .canonicalize()
            .map_err(|_| "native_start_home_unavailable")?;
        if companion.canonical_home().as_ref() != Some(&home) {
            return Err("native_start_home_mismatch".into());
        }
        let key = companion
            .admission_key_fingerprint()
            .ok_or("local_companion_offline")?;
        let owner = super::profile::native_owner_binding(&home, agent)?;
        let policy = crate::approval::policy_revision(&home, agent)?;
        Ok(proof::Binding {
            home,
            agent: agent.into(),
            key,
            owner,
            policy,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (companion, home, agent);
        Err("native_start_platform_unsupported".into())
    }
}

/// Only the authenticated native handler calls this after validating the
/// exact key and signed nonce. No user/person/admin identity is inferred.
pub(super) fn issue(
    companion: &Arc<Companion>,
    agent: &str,
    nonce: &str,
    verified_key: &str,
) -> Result<Permit, String> {
    let home = companion
        .canonical_home()
        .ok_or("native_start_home_unavailable")?;
    let expected = binding(companion, &home, agent)?;
    if expected.key != verified_key {
        return Err("native_start_key_changed".into());
    }
    let issuer = Arc::downgrade(companion);
    let weak = issuer.clone();
    let agent = agent.to_owned();
    let proof = proof::Permit::issue(
        expected,
        nonce.into(),
        Arc::new(move || {
            let companion = weak.upgrade().ok_or("local_companion_offline")?;
            binding(&companion, &home, &agent)
        }),
        Duration::from_secs(60),
    )?;
    Ok(Permit { proof, issuer })
}

impl Permit {
    pub(crate) fn claim(
        self,
        home: &Path,
        agent: &str,
        companion: &Arc<Companion>,
    ) -> Result<Authorization, String> {
        if !self
            .issuer
            .upgrade()
            .is_some_and(|issuer| Arc::ptr_eq(&issuer, companion))
        {
            return Err("native_start_key_instance_mismatch".into());
        }
        let home = home
            .canonicalize()
            .map_err(|_| "native_start_home_unavailable")?;
        let proof = self.proof.claim(&home, agent)?;
        Ok(Authorization {
            proof,
            issuer: self.issuer,
        })
    }
}
impl Authorization {
    pub(crate) fn check(
        &self,
        home: &Path,
        agent: &str,
        companion: &Arc<Companion>,
    ) -> Result<(), String> {
        if !self
            .issuer
            .upgrade()
            .is_some_and(|issuer| Arc::ptr_eq(&issuer, companion))
        {
            return Err("native_start_key_instance_mismatch".into());
        }
        let home = home
            .canonicalize()
            .map_err(|_| "native_start_home_unavailable")?;
        self.proof.check(&home, agent)
    }
    pub(crate) fn scope(&self) -> Scope {
        Scope {
            _proof: self.proof.scope(),
        }
    }
    pub(crate) fn nonce(&self) -> &str {
        self.proof.nonce()
    }
}
