//! Content-free diagnostics for user-facing delegation settings.
use super::{DelegationPolicy, MAX_ONE_WAY_PAIRS, UNRESTRICTABLE_ROLE_NAMES};
use serde::ser::{Serialize, SerializeStruct, Serializer};

/// Carries only a category and, for malformed pairs, their one-based position.
/// Legacy gate/log warnings remain available through the existing string APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationWarning {
    SectionInvalid,
    PolicyUnknown,
    PolicyNotString,
    AllowNotList,
    AllowPairInvalid(usize),
    AllowPairSameAgent(usize),
    OneWayNotList,
    OneWayPairInvalid(usize),
    OneWayPairSameAgent(usize),
    OneWayLimit,
    RolesUnknown,
}

impl DelegationWarning {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SectionInvalid => "section_invalid",
            Self::PolicyUnknown => "policy_unknown",
            Self::PolicyNotString => "policy_not_string",
            Self::AllowNotList => "allow_not_list",
            Self::AllowPairInvalid(_) => "allow_pair_invalid",
            Self::AllowPairSameAgent(_) => "allow_pair_same_agent",
            Self::OneWayNotList => "allow_one_way_not_list",
            Self::OneWayPairInvalid(_) => "allow_one_way_pair_invalid",
            Self::OneWayPairSameAgent(_) => "allow_one_way_pair_same_agent",
            Self::OneWayLimit => "allow_one_way_limit",
            Self::RolesUnknown => "unrestricted_roles_unknown",
        }
    }

    pub fn message_zh(&self) -> String {
        match self {
            Self::SectionInvalid => "委派設定格式有誤，已暫停員工之間的委派。".into(),
            Self::PolicyUnknown => "無法辨識委派模式，已暫停員工之間的委派。".into(),
            Self::PolicyNotString => "委派模式格式有誤，已暫停員工之間的委派。".into(),
            Self::AllowNotList => "委派配對清單格式有誤，已忽略。".into(),
            Self::AllowPairInvalid(n) => format!("第 {n} 組委派配對格式有誤，已忽略。"),
            Self::AllowPairSameAgent(n) => format!("第 {n} 組委派配對重複選取同一位員工，已忽略。"),
            Self::OneWayNotList => "單向委派配對清單格式有誤，已忽略。".into(),
            Self::OneWayPairInvalid(n) => format!("第 {n} 組單向委派配對格式有誤，已忽略。"),
            Self::OneWayPairSameAgent(n) => format!("第 {n} 組單向委派配對重複選取同一位員工，已忽略。"),
            Self::OneWayLimit => "單向委派配對超過 200 組，超出部分已忽略。".into(),
            Self::RolesUnknown => "不受委派範圍限制的角色設定含有無法辨識的項目，已忽略。".into(),
        }
    }
}

impl Serialize for DelegationWarning {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut warning = serializer.serialize_struct("DelegationWarning", 2)?;
        warning.serialize_field("code", self.code())?;
        warning.serialize_field("message", &self.message_zh())?;
        warning.end()
    }
}

/// Classify the original TOML shape; never parse a diagnostic string.
pub fn delegation_policy_warning(table: &toml::Table) -> Option<DelegationWarning> {
    let section = table.get("delegation")?;
    let Some(section) = section.as_table() else { return Some(DelegationWarning::SectionInvalid); };
    match section.get("policy")? {
        toml::Value::String(raw) if DelegationPolicy::parse(raw).is_some() => None,
        toml::Value::String(_) => Some(DelegationWarning::PolicyUnknown),
        _ => Some(DelegationWarning::PolicyNotString),
    }
}

fn pair_warnings(value: Option<&toml::Value>, directional: bool) -> Vec<DelegationWarning> {
    let Some(value) = value else { return Vec::new(); };
    let Some(entries) = value.as_array() else {
        return vec![if directional { DelegationWarning::OneWayNotList } else { DelegationWarning::AllowNotList }];
    };
    let mut warnings = Vec::new();
    let limit = if directional { MAX_ONE_WAY_PAIRS } else { entries.len() };
    for (index, entry) in entries.iter().take(limit).enumerate() {
        let pair = entry.as_array().filter(|a| a.len() == 2).and_then(|a| {
            let x = a[0].as_str()?.trim();
            let y = a[1].as_str()?.trim();
            if x.is_empty() || y.is_empty() { None } else { Some((x, y)) }
        });
        let warning = match pair {
            None if directional => Some(DelegationWarning::OneWayPairInvalid(index + 1)),
            None => Some(DelegationWarning::AllowPairInvalid(index + 1)),
            Some((x, y)) if x == y && directional => Some(DelegationWarning::OneWayPairSameAgent(index + 1)),
            Some((x, y)) if x == y => Some(DelegationWarning::AllowPairSameAgent(index + 1)),
            _ => None,
        };
        if let Some(warning) = warning { warnings.push(warning); }
    }
    if directional && entries.len() > limit { warnings.push(DelegationWarning::OneWayLimit); }
    warnings
}

pub fn delegation_section_warnings(table: &toml::Table) -> Vec<DelegationWarning> {
    let mut warnings: Vec<_> = delegation_policy_warning(table).into_iter().collect();
    if let Some(section) = table.get("delegation").and_then(toml::Value::as_table) {
        warnings.extend(pair_warnings(section.get("allow"), false));
    }
    warnings
}

pub fn delegation_override_warnings(table: &toml::Table) -> Vec<DelegationWarning> {
    let Some(section) = table.get("delegation").and_then(toml::Value::as_table) else { return Vec::new(); };
    let mut warnings = pair_warnings(section.get("allow_one_way"), true);
    let invalid_roles = match section.get("unrestricted_roles") {
        None => false,
        Some(toml::Value::Array(roles)) => roles.iter().any(|r| !r.as_str().is_some_and(|r| UNRESTRICTABLE_ROLE_NAMES.contains(&r))),
        Some(_) => true,
    };
    if invalid_roles { warnings.push(DelegationWarning::RolesUnknown); }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delegation_warning_codes_and_messages_are_content_free() {
        for (warning, code, message) in [
            (DelegationWarning::SectionInvalid, "section_invalid", "委派設定格式有誤，已暫停員工之間的委派。"),
            (DelegationWarning::PolicyUnknown, "policy_unknown", "無法辨識委派模式，已暫停員工之間的委派。"),
            (DelegationWarning::PolicyNotString, "policy_not_string", "委派模式格式有誤，已暫停員工之間的委派。"),
            (DelegationWarning::AllowNotList, "allow_not_list", "委派配對清單格式有誤，已忽略。"),
            (DelegationWarning::AllowPairInvalid(3), "allow_pair_invalid", "第 3 組委派配對格式有誤，已忽略。"),
            (DelegationWarning::AllowPairSameAgent(3), "allow_pair_same_agent", "第 3 組委派配對重複選取同一位員工，已忽略。"),
            (DelegationWarning::OneWayNotList, "allow_one_way_not_list", "單向委派配對清單格式有誤，已忽略。"),
            (DelegationWarning::OneWayPairInvalid(3), "allow_one_way_pair_invalid", "第 3 組單向委派配對格式有誤，已忽略。"),
            (DelegationWarning::OneWayPairSameAgent(3), "allow_one_way_pair_same_agent", "第 3 組單向委派配對重複選取同一位員工，已忽略。"),
            (DelegationWarning::OneWayLimit, "allow_one_way_limit", "單向委派配對超過 200 組，超出部分已忽略。"),
            (DelegationWarning::RolesUnknown, "unrestricted_roles_unknown", "不受委派範圍限制的角色設定含有無法辨識的項目，已忽略。"),
        ] {
            assert_eq!(warning.code(), code);
            assert_eq!(warning.message_zh(), message);
            assert_eq!(serde_json::to_value(&warning).unwrap(), json!({"code":code, "message":message}));
        }
    }

    #[test]
    fn delegation_warning_classification_preserves_resolution() {
        let cases = [
            ("delegation='raw-secret'", vec!["section_invalid"]),
            ("[delegation]\npolicy=42", vec!["policy_not_string"]),
            ("[delegation]\npolicy='weird-xyz'\nallow=[['sales-lead','sales-lead'],['solo'],[1,2],['','x']]", vec!["policy_unknown", "allow_pair_same_agent", "allow_pair_invalid", "allow_pair_invalid", "allow_pair_invalid"]),
            ("[delegation]\nallow='raw-secret'", vec!["allow_not_list"]),
            ("[delegation]\nallow_one_way='raw-secret'", vec!["allow_one_way_not_list"]),
            ("[delegation]\nallow_one_way=[['sales-lead','sales-lead'],['solo']]\nunrestricted_roles=['raw-role']", vec!["allow_one_way_pair_same_agent", "allow_one_way_pair_invalid", "unrestricted_roles_unknown"]),
        ];
        for (raw, codes) in cases {
            let table: toml::Table = raw.parse().unwrap();
            let cfg = super::super::delegation_config_from_table(&table);
            let before = (cfg.resolve(), cfg.resolve_overrides(), cfg.resolve_rules());
            let warnings: Vec<_> = delegation_section_warnings(&table).into_iter().chain(delegation_override_warnings(&table)).collect();
            assert_eq!(warnings.iter().map(DelegationWarning::code).collect::<Vec<_>>(), codes);
            let payload = serde_json::to_string(&warnings).unwrap();
            for raw in ["raw-secret", "weird-xyz", "sales-lead", "raw-role", "config.toml"] { assert!(!payload.contains(raw)); }
            assert_eq!(before, (cfg.resolve(), cfg.resolve_overrides(), cfg.resolve_rules()));
        }
    }

    #[test]
    fn delegation_warning_limits_and_valid_entries_match_legacy_parser() {
        let mut table: toml::Table = "[delegation]\npolicy='open'\nallow=[['a','b'],['b','a']]\nunrestricted_roles=['admin','manager']".parse().unwrap();
        assert!(delegation_section_warnings(&table).is_empty());
        assert!(delegation_override_warnings(&table).is_empty());
        table["delegation"].as_table_mut().unwrap().insert("allow_one_way".into(), toml::Value::Array(vec![toml::Value::Array(vec![toml::Value::String("a".into()),toml::Value::String("b".into())]); super::super::MAX_ONE_WAY_PAIRS + 1]));
        assert_eq!(delegation_override_warnings(&table), vec![DelegationWarning::OneWayLimit]);
        assert_eq!(super::super::delegation_config_from_table(&table).resolve_overrides().1.len(), 1);
    }
}
