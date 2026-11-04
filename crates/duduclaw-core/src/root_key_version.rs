//! Shared version policy for root-managed native installs and downloads.

/// Stable X.Y.Z releases only. The floor defaults to the running version;
/// installation may repeat that version, but upgrade must strictly advance it.
pub fn check_protected_version(target: &str, current: &str, floor: Option<&str>, upgrade: bool) -> Result<(), String> {
    let parse = |value: &str| {
        let version = semver::Version::parse(value).map_err(|_| "Invalid protected service version".to_string())?;
        if !version.pre.is_empty() || !version.build.is_empty() {
            return Err("Protected service requires a stable X.Y.Z version".to_string());
        }
        Ok(version)
    };
    let target = parse(target)?;
    let current = parse(current)?;
    let floor = match floor { Some(value) => parse(value)?, None => current.clone() };
    if target < floor || (upgrade && target <= current) {
        return Err("Protected upgrade/install refuses equal or older upgrade versions and versions below the installed floor".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_key_versions_refuse_replay_and_respect_floor() {
        for target in ["1.79.2", "1.79.3"] {
            assert!(check_protected_version(target, "1.79.3", None, true).is_err());
        }
        for target in ["1.79.4", "1.80.0", "2.0.0"] {
            assert!(check_protected_version(target, "1.79.3", None, true).is_ok());
        }
        assert!(check_protected_version("1.79.4", "1.79.3", Some("1.80.0"), true).is_err());
        assert!(check_protected_version("1.80.0", "1.79.3", Some("1.80.0"), true).is_ok());
        assert!(check_protected_version("1.79.3", "1.79.3", None, false).is_ok());
        assert!(check_protected_version("1.79.3", "1.79.3", Some("1.80.0"), false).is_err());
        for floor in ["", "bad", "1.2", "01.2.3", "1.2.3\n", "1.2.3-beta", "1.2.3+build"] {
            assert!(check_protected_version("2.0.0", "1.79.3", Some(floor), true).is_err(), "{floor:?}");
        }
        for target in ["latest", "v2.0.0", "2.0.0-beta", "2.0.0+build", "02.0.0"] {
            assert!(check_protected_version(target, "1.79.3", None, true).is_err());
        }
    }
}
