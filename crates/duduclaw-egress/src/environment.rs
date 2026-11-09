//! A/B1 must use Command::env_clear() before applying this root-selected map.
use std::{collections::BTreeMap, num::NonZeroU16};
use duduclaw_isolation::egress::EgressError;

/// Accept only the caller's trusted allowlist, never std::env::vars(). Proxy
/// names in ANY case are removed, including vendor-specific *_PROXY names.
/// This helper does not make arbitrary non-proxy variables safe to inherit.
pub fn child_environment(root_allowlist: &BTreeMap<String, String>, port: Option<NonZeroU16>)
    -> Result<BTreeMap<String, String>, EgressError> {
    let mut env = BTreeMap::new();
    for (key, value) in root_allowlist {
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || value.as_bytes().contains(&0) { return Err(EgressError::InvalidRequest); }
        let upper = key.to_ascii_uppercase();
        if upper == "PROXY" || upper.strip_suffix("_PROXY").is_some() { continue; }
        env.insert(key.clone(), value.clone());
    }
    if let Some(port) = port {
        let value = format!("http://127.0.0.1:{port}");
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env.insert(key.into(), value.clone());
        }
    }
    Ok(env)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_environment_is_case_insensitive_and_offline_cleans_all() {
        let original = ["hTtP_pRoXy", "NO_PROXY", "no_proxy", "All_Proxy", "ftp_proxy", "npm_config_proxy", "PROXY"]
            .into_iter().map(|key| (key.to_owned(), "secret".into())).chain([("HOME".into(), "/home/employee".into())]).collect();
        let clean = child_environment(&original, NonZeroU16::new(8080)).unwrap();
        assert_eq!(clean.len(), 5);
        assert!(clean.values().all(|value| value != "secret"));
        assert_eq!(child_environment(&original, None).unwrap().len(), 1);
    }
}
