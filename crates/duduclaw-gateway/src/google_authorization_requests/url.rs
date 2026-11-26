use std::path::Path;
use super::store::REFUSED;
pub(crate) const LANDING: &str = "me/google/authorize";
pub(crate) fn landing_base(home:&Path,channel:&str) -> Result<Option<String>,String> {
    let base=crate::deep_link::configured_public_base(home).map_err(|_|REFUSED)?;
    let Some(base)=base else {
        return Ok((channel=="webchat").then(||format!("/{LANDING}")));
    };
    if channel!="webchat" && !external_base(&base) { return Err(REFUSED.into()); }
    Ok(Some(format!("{}{LANDING}",base.as_str())))
}
pub(crate) fn external_base(url:&url::Url) -> bool {
    if url.scheme()!="https" || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some() { return false; }
    match url.host() {
        Some(url::Host::Domain(host)) => {
            let host=host.trim_end_matches('.');
            host!="localhost" && !host.ends_with(".localhost") && host.contains('.')
        }
        Some(url::Host::Ipv4(ip)) => !ip.is_loopback() && !ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => !ip.is_loopback() && !ip.is_unspecified()
            && ip.to_ipv4_mapped().is_none_or(|v|!v.is_loopback() && !v.is_unspecified()),
        None=>false,
    }
}
