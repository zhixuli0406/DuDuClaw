//! Native exact-purpose metadata. The selected singleton witnesses the
//! existing any-of policy; it never changes the OAuth grant itself.
use super::*;

#[cfg(test)]
#[path = "native_selection_tests.rs"]
mod tests;

/// Sealed metadata from one original SourceOwner selection. No raw receipt or
/// witness getter lets a consumer substitute its own material purpose.
pub(crate) struct NativeHomeSelection {
    receipt: HomeSourceReceipt,
    purpose: NativeHomePurpose,
}
enum NativeHomePurpose {
    Http {
        tool: String,
        singleton_scope: String,
    },
    MetadataStatus,
}
impl NativeHomeSelection {
    pub(crate) fn is_metadata_status(&self, actual_tool: &str) -> Result<bool> {
        match &self.purpose {
            NativeHomePurpose::MetadataStatus if actual_tool == "google_status" => Ok(true),
            NativeHomePurpose::Http { tool, .. } if tool == actual_tool => Ok(false),
            _ => Err(Error::Invalid),
        }
    }
    /// Nonsecret logical description, never a raw receipt or scope getter.
    pub(crate) fn logical_connector(&self) -> (&str, u64, &str) {
        (
            &self.receipt.credential_id,
            self.receipt.logical_epoch,
            &self.receipt.source_generation,
        )
    }
    pub(crate) fn storage_weight(&self) -> Result<usize> {
        let mut bytes = std::mem::size_of_val(self);
        let mut add = |n: usize| -> Result<()> {
            bytes = bytes.checked_add(n).ok_or(Error::Invalid)?;
            Ok(())
        };
        add(self.receipt.credential_id.capacity())?;
        add(self.receipt.source_generation.capacity())?;
        add(self
            .receipt
            .scopes
            .capacity()
            .checked_mul(std::mem::size_of::<String>())
            .ok_or(Error::Invalid)?)?;
        for scope in &self.receipt.scopes {
            add(scope.capacity())?;
        }
        if let NativeHomePurpose::Http {
            tool,
            singleton_scope,
        } = &self.purpose
        {
            add(tool.capacity())?;
            add(singleton_scope.capacity())?;
        }
        Ok(bytes)
    }
    /// Owner-computed digest of the complete original receipt and purpose.
    /// Accepts no caller hash and cannot issue a receipt/claim on its own.
    pub(crate) fn binding_digest(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let purpose = match &self.purpose {
            NativeHomePurpose::Http {
                tool,
                singleton_scope,
            } => json!({"http_tool":tool,"scope":singleton_scope}),
            NativeHomePurpose::MetadataStatus => json!({"metadata_status":true}),
        };
        let bytes=serde_json::to_vec(&json!({"domain":"native-home-original-v1","home":self.receipt.canonical_home,
            "backend":"oauth","credential_id":self.receipt.credential_id,"epoch":self.receipt.logical_epoch,
            "generation":self.receipt.source_generation,"client":self.receipt.client_stamp,
            "source":self.receipt.source_stamp,"scopes":self.receipt.scopes,"purpose":purpose})).map_err(|_|Error::Invalid)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
    fn http_scope(&self, actual_tool: &str) -> Result<&str> {
        match &self.purpose {
            NativeHomePurpose::Http {
                tool,
                singleton_scope,
            } if tool == actual_tool => Ok(singleton_scope),
            _ => Err(Error::Invalid),
        }
    }
}

/// Call only for the true host-bound tool. There is no caller-provided scope,
/// receipt or account parameter, and no alternate backend is attempted.
pub(crate) fn prepare_home_native_selection(
    home: &Path,
    tool: &str,
) -> Result<NativeHomeSelection> {
    if duduclaw_core::google_identity::native_google_scope(tool).is_none() {
        return Err(Error::Invalid);
    }
    with_metadata(home, |g, source| {
        if !source.master {
            return Err(Error::NotReady);
        }
        if source.sa_present {
            return Err(Error::PendingWiring);
        }
        let (receipt, purpose) = if tool == "google_status" {
            // Empty scopes are metadata-only here, never a material purpose.
            (
                selection(g, source, &[], true)?,
                NativeHomePurpose::MetadataStatus,
            )
        } else {
            let scope = home_store::native_scope_witness(g, source, tool)?;
            let receipt = selection(g, source, &[scope.as_str()], true)?;
            (
                receipt,
                NativeHomePurpose::Http {
                    tool: tool.into(),
                    singleton_scope: scope,
                },
            )
        };
        Ok(NativeHomeSelection { receipt, purpose })
    })
}

/// Pure logical validation for a consumed metadata/status host claim.
/// A material consumer must additionally use the exact proof validator below.
pub(crate) fn validate_home_native_selection(
    home: &Path,
    original: &NativeHomeSelection,
    actual_tool: &str,
) -> Result<()> {
    original.is_metadata_status(actual_tool)?;
    validate_home_source(home, &original.receipt)
}

pub(crate) async fn resolve_home_native_pinned(
    home: &Path,
    original: &NativeHomeSelection,
    actual_tool: &str,
) -> Result<HomeOAuthResolved> {
    // MetadataStatus and a different native tool fail before entering C1,
    // including its local refresh mutex or any material/key/provider work.
    let scope = original.http_scope(actual_tool)?;
    #[cfg(test)]
    testing::entered(home, 0);
    resolve_home_oauth_pinned(home, &original.receipt, &[scope]).await
}

pub(crate) fn validate_home_native_material(
    home: &Path,
    original: &NativeHomeSelection,
    actual_tool: &str,
    proof: &HomeOAuthMaterialProof,
) -> Result<()> {
    let scope = original.http_scope(actual_tool)?;
    #[cfg(test)]
    testing::entered(home, 1);
    validate_home_oauth_material(home, &original.receipt, proof, &[scope])
}

/// Count real C1 call boundaries, not a replacement resolver or validator.
/// Zero entries proves that status cannot reach the C1 material lane.
#[cfg(test)]
mod testing {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    fn counters() -> &'static Mutex<HashMap<PathBuf, [usize; 2]>> {
        static COUNTERS: OnceLock<Mutex<HashMap<PathBuf, [usize; 2]>>> = OnceLock::new();
        COUNTERS.get_or_init(Default::default)
    }
    pub(super) fn entered(home: &Path, index: usize) {
        counters()
            .lock()
            .unwrap()
            .entry(home.canonicalize().unwrap())
            .or_default()[index] += 1;
    }
    pub(super) fn resolver_entries(home: &Path) -> usize {
        counters()
            .lock()
            .unwrap()
            .get(&home.canonicalize().unwrap())
            .copied()
            .unwrap_or_default()[0]
    }
    pub(super) fn validator_entries(home: &Path) -> usize {
        counters()
            .lock()
            .unwrap()
            .get(&home.canonicalize().unwrap())
            .copied()
            .unwrap_or_default()[1]
    }
}

pub(super) async fn resolve_preflight<'a>(
    home: &Path,
    selected: &NativeHomeSelection,
    tool: &str,
    deadline: tokio::time::Instant,
    permit: &'a crate::google_identity::effect_authority::GooglePreflightPermit<'a>,
) -> Result<HomeOAuthResolved> {
    let scope = selected.http_scope(tool)?;
    oauth_material::resolve_home_oauth_preflight(
        home,
        &selected.receipt,
        &[scope],
        deadline,
        permit,
    )
    .await
}
