//! Fail-closed Drive write configuration and pure policy decisions.
//!
//! Only `load` and `tool_available` read configuration; this module never writes
//! configuration or contacts Google. An ack records consent, not a secret MAC.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveWriteAction {
    Create,
    Modify,
    Move,
    Trash,
}

const ACTIONS: [DriveWriteAction; 4] = [
    DriveWriteAction::Create,
    DriveWriteAction::Modify,
    DriveWriteAction::Move,
    DriveWriteAction::Trash,
];

impl DriveWriteAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Modify => "modify",
            Self::Move => "move",
            Self::Trash => "trash",
        }
    }
}

pub const DRIVE_WRITE_TOOLS: &[&str] = duduclaw_core::google_identity::DRIVE_WRITE_TOOLS;

pub fn action_for_tool(tool: &str) -> Option<DriveWriteAction> {
    match tool {
        "drive_create_file" | "drive_copy_file" => Some(DriveWriteAction::Create),
        "drive_update_file" | "docs_insert_text" | "docs_format"
        | "sheets_add_sheet" | "sheets_add_note" => Some(DriveWriteAction::Modify),
        "drive_move_file" => Some(DriveWriteAction::Move),
        "drive_trash_file" => Some(DriveWriteAction::Trash),
        _ => None,
    }
}

/// Declaration order also defines increasing approval strictness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    None,
    Member,
    MemberAndAdmin,
}

impl ApprovalMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Member => "member",
            Self::MemberAndAdmin => "member_and_admin",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderKind {
    Folder,
    SharedDrive,
    Unverified,
}

impl FolderKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::SharedDrive => "shared_drive",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub id: String,
    pub name: String,
    pub kind: FolderKind,
}

/// All four fields are always populated, including defaulted entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApprovalPolicy {
    pub create: ApprovalMode,
    pub modify: ApprovalMode,
    #[serde(rename = "move")]
    pub move_: ApprovalMode,
    pub trash: ApprovalMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ack {
    pub by_user_id: String,
    pub by_email: String,
    pub by_role: String,
    pub at: String,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveWritePolicy {
    pub create: bool,
    pub modify: bool,
    pub move_: bool,
    pub trash: bool,
    pub folders: Vec<FolderEntry>,
    pub approval: ApprovalPolicy,
    pub ack: Option<Ack>,
}

impl DriveWritePolicy {
    pub fn all_off() -> Self {
        Self {
            create: false,
            modify: false,
            move_: false,
            trash: false,
            folders: Vec::new(),
            approval: ApprovalPolicy {
                create: ApprovalMode::Member,
                modify: ApprovalMode::Member,
                move_: ApprovalMode::Member,
                trash: ApprovalMode::Member,
            },
            ack: None,
        }
    }

    fn configured(&self, action: DriveWriteAction) -> bool {
        match action {
            DriveWriteAction::Create => self.create,
            DriveWriteAction::Modify => self.modify,
            DriveWriteAction::Move => self.move_,
            DriveWriteAction::Trash => self.trash,
        }
    }

    pub fn action_enabled(&self, action: DriveWriteAction) -> bool {
        self.configured(action)
            && (!matches!(action, DriveWriteAction::Move | DriveWriteAction::Trash)
                || !self.folders.is_empty())
    }

    pub fn approval_for(&self, action: DriveWriteAction) -> ApprovalMode {
        match action {
            DriveWriteAction::Create => self.approval.create,
            DriveWriteAction::Modify => self.approval.modify,
            DriveWriteAction::Move => self.approval.move_,
            DriveWriteAction::Trash => self.approval.trash,
        }
    }

    pub fn any_enabled(&self) -> bool {
        ACTIONS.into_iter().any(|action| self.action_enabled(action))
    }
}

fn valid_id(id: &str) -> bool {
    (10..=128).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_text(text: &str, max_chars: usize, nonempty: bool) -> bool {
    (!nonempty || !text.is_empty())
        && text.chars().count() <= max_chars
        && !text.chars().any(char::is_control)
}

fn known_keys(table: &toml::Table, allowed: &[&str]) -> Option<()> {
    table.keys().all(|key| allowed.iter().any(|allowed| key.as_str() == *allowed)).then_some(())
}

fn bool_or_false(table: &toml::Table, key: &str) -> Option<bool> {
    match table.get(key) {
        None => Some(false),
        Some(value) => value.as_bool(),
    }
}

fn approval_mode(table: &toml::Table, key: &str) -> Option<ApprovalMode> {
    match table.get(key) {
        None => Some(ApprovalMode::Member),
        Some(value) => match value.as_str()? {
            "none" => Some(ApprovalMode::None),
            "member" => Some(ApprovalMode::Member),
            "member_and_admin" => Some(ApprovalMode::MemberAndAdmin),
            _ => None,
        },
    }
}

fn parse_ack(value: &toml::Value) -> Option<Ack> {
    let table = value.as_table()?;
    known_keys(table, &["by_user_id", "by_email", "by_role", "at", "digest"])?;
    let ack = Ack {
        by_user_id: table.get("by_user_id")?.as_str()?.to_owned(),
        by_email: table.get("by_email")?.as_str()?.to_owned(),
        by_role: table.get("by_role")?.as_str()?.to_owned(),
        at: table.get("at")?.as_str()?.to_owned(),
        digest: table.get("digest")?.as_str()?.to_owned(),
    };
    if ack.by_role != "admin"
        || !valid_text(&ack.by_user_id, 256, true)
        || !valid_text(&ack.by_email, 256, true)
        || DateTime::parse_from_rfc3339(&ack.at).is_err()
    {
        return None;
    }
    let hex = ack.digest.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    Some(ack)
}

fn parse_checked(section: &toml::Value) -> Option<DriveWritePolicy> {
    let table = section.as_table()?;
    known_keys(table, &["create", "modify", "move", "trash", "folders", "approval", "ack"])?;
    let mut policy = DriveWritePolicy::all_off();
    policy.create = bool_or_false(table, "create")?;
    policy.modify = bool_or_false(table, "modify")?;
    policy.move_ = bool_or_false(table, "move")?;
    policy.trash = bool_or_false(table, "trash")?;
    if let Some(value) = table.get("folders") {
        let folders = value.as_array()?;
        if folders.len() > 50 {
            return None;
        }
        let mut ids = HashSet::new();
        for value in folders {
            let entry = value.as_table()?;
            known_keys(entry, &["id", "name", "kind"])?;
            let id = entry.get("id")?.as_str()?;
            let name = entry.get("name")?.as_str()?;
            if !valid_id(id) || !ids.insert(id) || !valid_text(name, 200, false) {
                return None;
            }
            let kind = match entry.get("kind")?.as_str()? {
                "folder" => FolderKind::Folder,
                "shared_drive" => FolderKind::SharedDrive,
                "unverified" => FolderKind::Unverified,
                _ => return None,
            };
            policy.folders.push(FolderEntry { id: id.to_owned(), name: name.to_owned(), kind });
        }
    }
    if let Some(value) = table.get("approval") {
        let approval = value.as_table()?;
        known_keys(approval, &["create", "modify", "move", "trash"])?;
        policy.approval = ApprovalPolicy {
            create: approval_mode(approval, "create")?,
            modify: approval_mode(approval, "modify")?,
            move_: approval_mode(approval, "move")?,
            trash: approval_mode(approval, "trash")?,
        };
    }
    if let Some(value) = table.get("ack") {
        policy.ack = Some(parse_ack(value)?);
    }
    // Check configured switches, even if empty folders make move/trash unavailable.
    if ACTIONS.into_iter().any(|action| policy.configured(action))
        && policy.ack.as_ref()?.digest != digest_of(&policy)
    {
        return None;
    }
    Some(policy)
}

pub fn parse_section(
    section: Option<&toml::Value>,
    google_workspace_integration_enabled: bool,
) -> DriveWritePolicy {
    if !google_workspace_integration_enabled {
        return DriveWritePolicy::all_off();
    }
    section.and_then(parse_checked).unwrap_or_else(DriveWritePolicy::all_off)
}

pub fn load(home: &Path) -> DriveWritePolicy {
    if !super::integration_enabled(home) {
        return DriveWritePolicy::all_off();
    }
    let Ok(raw) = std::fs::read_to_string(home.join("config.toml")) else {
        return DriveWritePolicy::all_off();
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return DriveWritePolicy::all_off();
    };
    // Also require the flag in the snapshot containing the section being parsed.
    let enabled = table.get("integrations")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("google_workspace"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    parse_section(table.get("google").and_then(|v| v.get("drive_write")), enabled)
}

/// Hash compact JSON with the field order below, independent of map features.
/// Struct serialization fixes key order; ack is deliberately absent.
pub fn digest_of(policy: &DriveWritePolicy) -> String {
    #[derive(Serialize)]
    struct CanonicalFolder<'a> {
        id: &'a str,
        kind: FolderKind,
        name: &'a str,
    }
    #[derive(Serialize)]
    struct CanonicalPolicy<'a> {
        create: bool,
        modify: bool,
        #[serde(rename = "move")]
        move_: bool,
        trash: bool,
        folders: Vec<CanonicalFolder<'a>>,
        approval: &'a ApprovalPolicy,
    }
    let canonical = CanonicalPolicy {
        create: policy.create,
        modify: policy.modify,
        move_: policy.move_,
        trash: policy.trash,
        folders: policy.folders.iter().map(|folder| CanonicalFolder {
            id: &folder.id,
            kind: folder.kind,
            name: &folder.name,
        }).collect(),
        approval: &policy.approval,
    };
    // Serialization contains only booleans, strings, arrays and fixed structs.
    let bytes = serde_json::to_vec(&canonical).expect("canonical Drive policy is serializable");
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub fn tool_available(home: &Path, tool: &str) -> bool {
    action_for_tool(tool).is_some_and(|action| load(home).action_enabled(action))
}

/// Compare configured switches, so enabling a currently unusable action still
/// requires consent. Approval lowering is widening even for disabled actions.
pub fn widening(old: &DriveWritePolicy, new: &DriveWritePolicy) -> bool {
    ACTIONS.into_iter().any(|action| {
        (!old.configured(action) && new.configured(action))
            || new.approval_for(action) < old.approval_for(action)
    }) || new.folders.iter().any(|folder| !old.folders.iter().any(|old| old.id == folder.id))
        || (!old.folders.is_empty() && new.folders.is_empty() && (new.create || new.modify))
}

pub fn extract_folder_id(input: &str) -> Option<String> {
    let input = input.trim();
    if valid_id(input) {
        return Some(input.to_owned());
    }
    // URL parsing normalizes dot segments and strips some controls; reject them
    // first instead of validating a different path than the user supplied.
    if input.chars().any(|c| c.is_control() || c.is_whitespace())
        || input.split(['/', '?', '#']).any(|part| part == "." || part == "..")
    {
        return None;
    }
    let (scheme, _) = input.split_once("://")?;
    if scheme != "https" || input.chars().any(|c| c == '\\') {
        return None;
    }
    let url = url::Url::parse(input).ok()?;
    if url.scheme() != "https" || url.host_str() != Some("drive.google.com")
        || !url.username().is_empty() || url.password().is_some() || url.port().is_some()
    {
        return None;
    }
    let ids: Vec<_> = url.query_pairs().filter(|(key, _)| key == "id")
        .map(|(_, value)| value.into_owned()).collect();
    if !ids.is_empty() {
        return (ids.len() == 1 && valid_id(&ids[0])).then(|| ids[0].clone());
    }
    let segments: Vec<_> = url.path_segments()?.collect();
    let id = match segments.as_slice() {
        ["drive", "folders", id] => *id,
        ["drive", "u", account, "folders", id]
            if !account.is_empty() && account.bytes().all(|b| b.is_ascii_digit()) => *id,
        _ => return None,
    };
    valid_id(id).then(|| id.to_owned())
}

/// Produce just the `google.drive_write` table; callers own persistence.
pub fn render_section(policy: &DriveWritePolicy, ack: &Ack) -> toml::Value {
    fn string(value: &str) -> toml::Value {
        toml::Value::String(value.to_owned())
    }
    let mut section = toml::Table::new();
    let mut approval = toml::Table::new();
    for action in ACTIONS {
        section.insert(action.as_str().to_owned(), toml::Value::Boolean(policy.configured(action)));
        approval.insert(action.as_str().to_owned(), string(policy.approval_for(action).as_str()));
    }
    section.insert("approval".to_owned(), toml::Value::Table(approval));
    section.insert("folders".to_owned(), toml::Value::Array(policy.folders.iter().map(|folder| {
        let mut entry = toml::Table::new();
        entry.insert("id".to_owned(), string(&folder.id));
        entry.insert("name".to_owned(), string(&folder.name));
        entry.insert("kind".to_owned(), string(folder.kind.as_str()));
        toml::Value::Table(entry)
    }).collect()));
    let mut ack_table = toml::Table::new();
    for (key, value) in [
        ("by_user_id", &ack.by_user_id), ("by_email", &ack.by_email),
        ("by_role", &ack.by_role), ("at", &ack.at), ("digest", &ack.digest),
    ] {
        ack_table.insert(key.to_owned(), string(value));
    }
    section.insert("ack".to_owned(), toml::Value::Table(ack_table));
    toml::Value::Table(section)
}

pub fn sign(
    policy: &DriveWritePolicy,
    by_user_id: &str,
    by_email: &str,
    at: DateTime<Utc>,
) -> Ack {
    Ack {
        by_user_id: by_user_id.to_owned(),
        by_email: by_email.to_owned(),
        by_role: "admin".to_owned(),
        at: at.to_rfc3339(),
        digest: digest_of(policy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "AbCdEf_012345-6789";
    const OTHER_ID: &str = "Another_0123456789";

    fn folder(id: &str) -> FolderEntry {
        FolderEntry { id: id.to_owned(), name: "共用資料夾".to_owned(), kind: FolderKind::Folder }
    }

    fn ack(policy: &DriveWritePolicy) -> Ack {
        sign(policy, "admin-id", "admin@example.com", DateTime::parse_from_rfc3339(
            "2026-11-27T12:00:00+08:00",
        ).unwrap().with_timezone(&Utc))
    }

    fn signed_section(policy: &DriveWritePolicy) -> toml::Value {
        render_section(policy, &ack(policy))
    }

    fn config_text(section: &toml::Value, enabled: bool) -> String {
        let mut google = toml::Table::new();
        google.insert("drive_write".to_owned(), section.clone());
        let mut integrations = toml::Table::new();
        integrations.insert("google_workspace".to_owned(), toml::Value::Boolean(enabled));
        let mut root = toml::Table::new();
        root.insert("google".to_owned(), toml::Value::Table(google));
        root.insert("integrations".to_owned(), toml::Value::Table(integrations));
        toml::to_string(&root).unwrap()
    }

    fn enabled_policy() -> DriveWritePolicy {
        let mut policy = DriveWritePolicy::all_off();
        policy.create = true;
        policy.modify = true;
        policy.move_ = true;
        policy.trash = true;
        policy.folders.push(folder(ID));
        policy
    }

    fn assert_off(section: &toml::Value) {
        assert_eq!(parse_section(Some(section), true), DriveWritePolicy::all_off());
    }

    #[test]
    fn missing_non_table_and_unknown_keys_fail_closed() {
        assert_eq!(parse_section(None, true), DriveWritePolicy::all_off());
        assert_off(&toml::Value::String("bad".to_owned()));
        assert_off(&toml::Value::Boolean(true));
        // Keep switches off so invalid-field tests do not merely fail the digest.
        let mut policy = DriveWritePolicy::all_off();
        policy.folders.push(folder(ID));
        for path in [vec![], vec!["approval"], vec!["ack"]] {
            let mut section = signed_section(&policy);
            let mut target = &mut section;
            for key in path { target = target.get_mut(key).unwrap(); }
            target.as_table_mut().unwrap().insert("unknown".to_owned(), toml::Value::Boolean(false));
            assert_off(&section);
        }
        let mut section = signed_section(&policy);
        section["folders"][0].as_table_mut().unwrap()
            .insert("unknown".to_owned(), toml::Value::Boolean(false));
        assert_off(&section);
    }

    #[test]
    fn wrong_types_at_every_level_fail_closed() {
        let mut policy = DriveWritePolicy::all_off();
        policy.folders.push(folder(ID));
        for key in ["create", "modify", "move", "trash", "folders", "approval", "ack"] {
            let mut section = signed_section(&policy);
            section[key] = toml::Value::Integer(1);
            assert_off(&section);
        }
        for key in ["create", "modify", "move", "trash"] {
            let mut section = signed_section(&policy);
            section["approval"][key] = toml::Value::Boolean(true);
            assert_off(&section);
            section["approval"][key] = toml::Value::String("unknown".to_owned());
            assert_off(&section);
        }
        for key in ["id", "name", "kind"] {
            let mut section = signed_section(&policy);
            section["folders"][0][key] = toml::Value::Integer(1);
            assert_off(&section);
            section["folders"][0].as_table_mut().unwrap().remove(key);
            assert_off(&section);
        }
        let mut section = signed_section(&policy);
        section["folders"][0] = toml::Value::String(ID.to_owned());
        assert_off(&section);
        for key in ["by_user_id", "by_email", "by_role", "at", "digest"] {
            let mut section = signed_section(&policy);
            section["ack"][key] = toml::Value::Integer(1);
            assert_off(&section);
            section["ack"].as_table_mut().unwrap().remove(key);
            assert_off(&section);
        }
    }

    #[test]
    fn malformed_folders_fail_closed() {
        let mut policy = DriveWritePolicy::all_off();
        policy.folders.push(folder(ID));
        for id in ["short".to_owned(), "a".repeat(129), "AbCdEf/012345".to_owned(),
            "..".to_owned(), "中文資料夾識別碼".to_owned(), "AbCdEf_012345\n".to_owned()]
        {
            let mut invalid = policy.clone();
            invalid.folders[0].id = id;
            assert_off(&signed_section(&invalid));
        }
        for name in ["字".repeat(201), "bad\nname".to_owned(), "bad\u{7f}".to_owned()] {
            let mut invalid = policy.clone();
            invalid.folders[0].name = name;
            assert_off(&signed_section(&invalid));
        }
        let mut section = signed_section(&policy);
        section["folders"][0]["kind"] = toml::Value::String("shortcut".to_owned());
        assert_off(&section);
        let mut duplicate = policy.clone();
        duplicate.folders.push(folder(ID));
        assert_off(&signed_section(&duplicate));
        let mut too_many = policy.clone();
        too_many.folders = (0..51).map(|n| folder(&format!("Folder_{n:010}"))).collect();
        assert_off(&signed_section(&too_many));
        too_many.folders.pop();
        let section = signed_section(&too_many);
        assert_eq!(parse_section(Some(&section), true).folders.len(), 50);
        for id in ["a".repeat(10), "a".repeat(128)] {
            let mut valid = policy.clone();
            valid.folders[0].id = id;
            valid.folders[0].name = "字".repeat(200);
            let section = signed_section(&valid);
            assert_eq!(parse_section(Some(&section), true).folders, valid.folders);
        }
    }

    #[test]
    fn missing_and_malformed_ack_fail_closed_even_with_all_switches_off() {
        let mut enabled = DriveWritePolicy::all_off();
        enabled.move_ = true; // Ack required even though move is unavailable.
        let mut section = signed_section(&enabled);
        section.as_table_mut().unwrap().remove("ack");
        assert_off(&section);
        for action in ACTIONS {
            let mut section = signed_section(&DriveWritePolicy::all_off());
            section[action.as_str()] = toml::Value::Boolean(true);
            section.as_table_mut().unwrap().remove("ack");
            assert_off(&section);
        }
        let policy = DriveWritePolicy::all_off();
        for (key, values) in [
            ("by_user_id", vec![String::new(), "字".repeat(257), "a\nb".to_owned()]),
            ("by_email", vec![String::new(), "字".repeat(257), "a\u{7f}b".to_owned()]),
            ("by_role", vec!["member".to_owned(), "Admin".to_owned()]),
            ("at", vec!["yesterday".to_owned(), "2026-11-27".to_owned()]),
            ("digest", vec!["sha256:abc".to_owned(), format!("sha256:{}", "A".repeat(64)),
                format!("SHA256:{}", "a".repeat(64)), format!("sha256:{}", "g".repeat(64)),
                format!("sha256:{}", "a".repeat(65))]),
        ] {
            for value in values {
                let mut section = signed_section(&policy);
                section["ack"][key] = toml::Value::String(value);
                assert_off(&section);
            }
        }
        let mut valid = ack(&enabled);
        valid.by_user_id = "字".repeat(256);
        valid.by_email = "字".repeat(256);
        assert!(parse_section(Some(&render_section(&enabled, &valid)), true).move_);
    }

    #[test]
    fn hand_edited_switch_invalidates_digest_but_ack_time_does_not() {
        let mut policy = enabled_policy();
        policy.trash = false;
        let mut section = signed_section(&policy);
        section["trash"] = toml::Value::Boolean(true);
        assert_off(&section);
        let mut section = signed_section(&policy);
        section["ack"]["at"] = toml::Value::String("2027-01-01T00:00:00Z".to_owned());
        let parsed = parse_section(Some(&section), true);
        assert!(parsed.create);
        assert_eq!(parsed.ack.unwrap().at, "2027-01-01T00:00:00Z");
    }

    #[test]
    fn canonical_digest_defaults_and_key_order_are_stable() {
        let first: toml::Value = "create = false\nmodify = false\nmove = false\ntrash = false\nfolders = []"
            .parse().unwrap();
        let second: toml::Value = "approval = {trash='member', move='member', modify='member', create='member'}\nfolders=[]\ntrash=false\nmove=false\nmodify=false\ncreate=false"
            .parse().unwrap();
        let first = parse_section(Some(&first), true);
        let second = parse_section(Some(&second), true);
        assert_eq!(digest_of(&first), digest_of(&second));
        let json = r#"{"create":false,"modify":false,"move":false,"trash":false,"folders":[],"approval":{"create":"member","modify":"member","move":"member","trash":"member"}}"#;
        assert_eq!(digest_of(&first), format!("sha256:{}", hex::encode(Sha256::digest(json.as_bytes()))));
        let policy = enabled_policy();
        let mut section = signed_section(&policy);
        section["approval"].as_table_mut().unwrap().remove("modify");
        assert_eq!(parse_section(Some(&section), true).approval, policy.approval);
        section.as_table_mut().unwrap().remove("approval");
        let parsed = parse_section(Some(&section), true);
        assert!(parsed.create);
        assert_eq!(digest_of(&parsed), digest_of(&policy));
        let mut with_ack = policy.clone();
        with_ack.ack = Some(ack(&policy));
        assert_eq!(digest_of(&with_ack), digest_of(&policy));
        let mut ordered = policy;
        ordered.folders.push(folder(OTHER_ID));
        let mut reversed = ordered.clone();
        reversed.folders.reverse();
        assert_ne!(digest_of(&ordered), digest_of(&reversed));
    }

    #[test]
    fn empty_folders_only_disable_move_and_trash() {
        let mut policy = enabled_policy();
        for action in ACTIONS { assert!(policy.action_enabled(action)); }
        policy.folders.clear();
        assert!(policy.action_enabled(DriveWriteAction::Create));
        assert!(policy.action_enabled(DriveWriteAction::Modify));
        assert!(!policy.action_enabled(DriveWriteAction::Move));
        assert!(!policy.action_enabled(DriveWriteAction::Trash));
        assert!(policy.any_enabled());
        policy.create = false;
        policy.modify = false;
        assert!(!policy.any_enabled());
        assert!(!DriveWritePolicy::all_off().any_enabled());
    }

    #[test]
    fn tool_mapping_is_exact_and_excludes_legacy_tools() {
        let expected = [DriveWriteAction::Create, DriveWriteAction::Create,
            DriveWriteAction::Modify, DriveWriteAction::Modify, DriveWriteAction::Modify,
            DriveWriteAction::Modify, DriveWriteAction::Modify, DriveWriteAction::Move,
            DriveWriteAction::Trash];
        for (tool, action) in DRIVE_WRITE_TOOLS.into_iter().zip(expected) {
            assert_eq!(action_for_tool(tool), Some(action));
            assert_eq!(action_for_tool(&format!("{tool}_extra")), None);
        }
        let dir = tempfile::tempdir().unwrap();
        let section = signed_section(&enabled_policy());
        std::fs::write(dir.path().join("config.toml"), config_text(&section, true)).unwrap();
        for tool in DRIVE_WRITE_TOOLS { assert!(tool_available(dir.path(), tool)); }
        for tool in ["unknown", "docs_append", "docs_create", "docs_replace_text",
            "sheets_append", "sheets_create", "sheets_update"]
        {
            assert_eq!(action_for_tool(tool), None);
            assert!(!tool_available(dir.path(), tool));
        }
    }

    #[test]
    fn integration_and_file_errors_fail_closed() {
        let section = signed_section(&enabled_policy());
        assert_eq!(parse_section(Some(&section), false), DriveWritePolicy::all_off());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), DriveWritePolicy::all_off());
        for raw in ["broken [", "[integrations]\ngoogle_workspace='true'",
            "[google.drive_write]\ncreate=true"]
        {
            std::fs::write(dir.path().join("config.toml"), raw).unwrap();
            assert_eq!(load(dir.path()), DriveWritePolicy::all_off());
        }
        std::fs::write(dir.path().join("config.toml"), config_text(&section, false)).unwrap();
        assert_eq!(load(dir.path()), DriveWritePolicy::all_off());
    }

    #[test]
    fn widening_detects_each_rule_and_rejects_narrowing() {
        let off = DriveWritePolicy::all_off();
        for action in ACTIONS {
            let mut new = off.clone();
            match action {
                DriveWriteAction::Create => new.create = true,
                DriveWriteAction::Modify => new.modify = true,
                DriveWriteAction::Move => new.move_ = true,
                DriveWriteAction::Trash => new.trash = true,
            }
            assert!(widening(&off, &new));
            assert!(!widening(&new, &off));
            for (strict, relaxed) in [
                (ApprovalMode::MemberAndAdmin, ApprovalMode::Member),
                (ApprovalMode::MemberAndAdmin, ApprovalMode::None),
                (ApprovalMode::Member, ApprovalMode::None),
            ] {
                let mut old = off.clone();
                let mut new = off.clone();
                match action {
                    DriveWriteAction::Create => { old.approval.create = strict; new.approval.create = relaxed; }
                    DriveWriteAction::Modify => { old.approval.modify = strict; new.approval.modify = relaxed; }
                    DriveWriteAction::Move => { old.approval.move_ = strict; new.approval.move_ = relaxed; }
                    DriveWriteAction::Trash => { old.approval.trash = strict; new.approval.trash = relaxed; }
                }
                assert!(widening(&old, &new));
                assert!(!widening(&new, &old));
            }
        }
        let mut scoped = off.clone();
        scoped.folders.push(folder(ID));
        assert!(widening(&off, &scoped));
        assert!(!widening(&scoped, &off));
        for create in [true, false] {
            let mut old = scoped.clone();
            old.create = create;
            old.modify = !create;
            let mut new = old.clone();
            new.folders.clear();
            assert!(widening(&old, &new));
        }
        let mut old = enabled_policy();
        old.folders.push(folder(OTHER_ID));
        let mut narrowed = old.clone();
        narrowed.folders.pop();
        narrowed.trash = false;
        assert!(!widening(&old, &narrowed));
        let mut metadata_only = old.clone();
        metadata_only.folders.reverse();
        metadata_only.folders[0].name = "Renamed".to_owned();
        metadata_only.folders[0].kind = FolderKind::Unverified;
        metadata_only.ack = Some(ack(&metadata_only));
        assert!(!widening(&old, &metadata_only));
        assert!(!widening(&old, &old));
    }

    #[test]
    fn extract_folder_id_accepts_documented_forms() {
        for input in [ID.to_owned(), format!("  {ID}\n"),
            format!("https://drive.google.com/drive/folders/{ID}"),
            format!("https://drive.google.com/drive/u/0/folders/{ID}"),
            format!("https://drive.google.com/drive/u/12/folders/{ID}"),
            format!("https://drive.google.com/drive/folders/{ID}?usp=sharing"),
            format!("https://drive.google.com/open?id={ID}"),
            format!("https://drive.google.com/anything?usp=sharing&id={ID}")]
        {
            assert_eq!(extract_folder_id(&input), Some(ID.to_owned()), "{input}");
        }
    }

    #[test]
    fn extract_folder_id_rejects_hosts_schemes_paths_and_invalid_ids() {
        for input in [
            format!("http://drive.google.com/drive/folders/{ID}"),
            format!("https://drive.google.com.evil.com/drive/folders/{ID}"),
            format!("https://evil.com/?id={ID}"),
            format!("https://drive.google.com@evil.com/?id={ID}"),
            format!("https://drive.google.com/drive/folders/{ID}/child"),
            format!("https://drive.google.com/drive/u/no/folders/{ID}"),
            format!("https://drive.google.com/drive/../drive/folders/{ID}"),
            format!("https://drive.google.com/?id={ID}&id={OTHER_ID}"),
            format!("https://drive.google.com/?id={ID}%2Fpermissions"),
            format!("https://drive.google.com/drive/folders/{ID}\tbad"),
            "AbCdEf/012345".to_owned(), "..".to_owned(), "short".to_owned(), "a".repeat(129),
            "https://drive.google.com/?id=..".to_owned(),
            "https://drive.google.com/drive/folders/short".to_owned(),
            format!("https://drive.google.com/?id={}", "a".repeat(129)),
        ] {
            assert_eq!(extract_folder_id(&input), None, "{input}");
        }
    }

    #[test]
    fn render_sign_parse_round_trip() {
        let mut policy = enabled_policy();
        policy.approval.create = ApprovalMode::None;
        policy.approval.modify = ApprovalMode::MemberAndAdmin;
        policy.folders.push(FolderEntry {
            id: OTHER_ID.to_owned(), name: String::new(), kind: FolderKind::Unverified,
        });
        policy.folders.push(FolderEntry {
            id: "Shared_drive_0123".to_owned(), name: "共用硬碟".to_owned(), kind: FolderKind::SharedDrive,
        });
        let ack = ack(&policy);
        let rendered = render_section(&policy, &ack);
        policy.ack = Some(ack);
        assert_eq!(parse_section(Some(&rendered), true), policy);
        // Exercise TOML serialization as well as the in-memory table.
        let text = toml::to_string(&rendered).unwrap();
        let reparsed: toml::Value = text.parse().unwrap();
        assert_eq!(parse_section(Some(&reparsed), true), policy);
        for action in ACTIONS {
            assert_eq!(rendered["approval"][action.as_str()].as_str(),
                Some(policy.approval_for(action).as_str()));
        }
    }
}
