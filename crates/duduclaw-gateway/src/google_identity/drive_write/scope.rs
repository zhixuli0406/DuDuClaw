//! Google-backed folder scope checks. One Budget belongs to one tool call.
//! This module checks scope and capabilities, not action switches or approvals.

use std::{collections::{BTreeSet, HashMap}, future::Future, pin::Pin};

use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use crate::google_workspace::drive_write_policy::{DriveWritePolicy, FolderEntry, FolderKind};
use super::transport::{GoogleId, MetadataFields, WriteCall, WriteResult, WriteTransport};

const MAX_REQUESTS: usize = 16;
const MAX_DEPTH: usize = 12;
const MAX_NAME_CHARS: usize = 256;
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaError { NotVisible, Failed }

pub(crate) trait MetadataSource: Send {
    fn get(&mut self, id: &GoogleId) -> BoxFuture<'_, Result<FileMeta, MetaError>>;
}

impl MetadataSource for WriteTransport {
    fn get(&mut self, id: &GoogleId) -> BoxFuture<'_, Result<FileMeta, MetaError>> {
        let id = id.clone();
        Box::pin(async move {
            let call = WriteCall::DriveGetMetadata {
                file_id: id.clone(), fields: MetadataFields::Scope,
            };
            metadata_result(self.send(&call).await, &id)
        })
    }
}

impl<S: MetadataSource + ?Sized> MetadataSource for &mut S {
    fn get(&mut self, id: &GoogleId) -> BoxFuture<'_, Result<FileMeta, MetaError>> {
        (**self).get(id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Caps {
    #[serde(default)]
    pub(crate) can_edit: bool,
    #[serde(default)]
    pub(crate) can_rename: bool,
    #[serde(default)]
    pub(crate) can_add_children: bool,
    #[serde(default)]
    pub(crate) can_trash: bool,
    #[serde(default)]
    pub(crate) can_move_item_within_drive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileMeta {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) mime_type: String,
    pub(crate) parents: Option<Vec<String>>,
    pub(crate) drive_id: Option<String>,
    pub(crate) trashed: bool,
    pub(crate) shortcut_target: Option<String>,
    pub(crate) capabilities: Caps,
}

// Missing optional fields are distinct from malformed/null fields. Never
// coerce strings/numbers into booleans or silently drop a malformed parent.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where D: Deserializer<'de>, T: Deserialize<'de> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMeta {
    id: String,
    name: String,
    mime_type: String,
    #[serde(default, deserialize_with = "present")]
    parents: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    drive_id: Option<String>,
    trashed: bool,
    #[serde(default, deserialize_with = "present")]
    shortcut_details: Option<WireShortcut>,
    #[serde(default)]
    capabilities: Caps,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireShortcut { target_id: String }

pub(super) fn metadata_result(result: WriteResult, expected: &GoogleId) -> Result<FileMeta, MetaError> {
    let body = match result {
        WriteResult::Ok((403 | 404, _)) => return Err(MetaError::NotVisible),
        WriteResult::Ok((200..=299, body)) => body,
        _ => return Err(MetaError::Failed),
    };
    let wire: WireMeta = serde_json::from_slice(&body).map_err(|_| MetaError::Failed)?;
    let meta = FileMeta {
        id: wire.id,
        name: duduclaw_core::truncate_chars(&wire.name, MAX_NAME_CHARS),
        mime_type: wire.mime_type,
        parents: wire.parents,
        drive_id: wire.drive_id,
        trashed: wire.trashed,
        shortcut_target: wire.shortcut_details.map(|details| details.target_id),
        capabilities: wire.capabilities,
    };
    validate_meta(&meta)?;
    if meta.id != expected.as_str() { return Err(MetaError::Failed); }
    Ok(meta)
}

fn validate_meta(meta: &FileMeta) -> Result<(), MetaError> {
    let valid_id = |id: &str| GoogleId::new(id).is_ok();
    if !valid_id(&meta.id) || meta.mime_type.is_empty()
        || meta.drive_id.as_deref().is_some_and(|id| !valid_id(id))
        || meta.shortcut_target.as_deref().is_some_and(|id| !valid_id(id))
        || (meta.mime_type == SHORTCUT_MIME && meta.shortcut_target.is_none())
    {
        return Err(MetaError::Failed);
    }
    if let Some(parents) = &meta.parents {
        let unique: BTreeSet<_> = parents.iter().collect();
        if unique.len() != parents.len() || parents.iter().any(|id| !valid_id(id)) {
            return Err(MetaError::Failed);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopeReason {
    FolderOutOfScope,
    ParentNotVisible,
    Trashed,
    ShortcutNotAllowed,
    CapabilityDenied,
    CrossDriveMove,
    ListedFolderProtected,
    FolderRequired,
}

impl ScopeReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::FolderOutOfScope => "folder_out_of_scope",
            Self::ParentNotVisible => "parent_not_visible",
            Self::Trashed => "trashed",
            Self::ShortcutNotAllowed => "shortcut_not_allowed",
            Self::CapabilityDenied => "capability_denied",
            Self::CrossDriveMove => "cross_drive_move",
            Self::ListedFolderProtected => "listed_folder_protected",
            Self::FolderRequired => "folder_required",
        }
    }

    pub(crate) fn user_text(self) -> &'static str {
        match self {
            Self::FolderOutOfScope => "這個位置不在允許使用的資料夾內，這次沒有執行，可請管理員調整允許使用的資料夾。",
            Self::ParentNotVisible => "無法確認上層資料夾，這次沒有執行，請帳號本人確認存取權限。",
            Self::Trashed => "檔案或上層資料夾已在垃圾桶中，這次沒有執行，請帳號本人先還原。",
            Self::ShortcutNotAllowed => "這是捷徑，無法執行這項操作，請改用原始檔案。",
            Self::CapabilityDenied => "目前的 Google 帳號沒有執行這項操作的權限，可請檔案擁有者調整分享權限。",
            Self::CrossDriveMove => "無法跨雲端硬碟搬移檔案，請選擇同一個雲端硬碟內的目的地。",
            Self::ListedFolderProtected => "無法改名、搬移或刪除管理員指定的可用資料夾，請管理員調整可用資料夾後再試。",
            Self::FolderRequired => "這項操作需要先限定可使用的資料夾，請管理員設定。",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScopeVerdict {
    InScope {
        matched: Vec<String>,
        /// The checked Google object, or None for implicit My Drive creation.
        /// Copy binds the source; Create with a parent binds that parent.
        target_id: Option<String>,
        /// Current Google parents, NEVER caller-provided removeParents.
        parents: Option<Vec<String>>,
    },
    OutOfScope { reason: ScopeReason },
    LimitExceeded,
    Failed,
}

impl ScopeVerdict {
    pub(crate) fn reason_code(&self) -> Option<&'static str> {
        match self {
            Self::OutOfScope { reason } => Some(reason.code()),
            Self::LimitExceeded => Some("folder_check_limit"),
            Self::InScope { .. } | Self::Failed => None,
        }
    }
}

fn denied(reason: ScopeReason) -> ScopeVerdict { ScopeVerdict::OutOfScope { reason } }

fn allowed(target: &FileMeta, matched: Vec<String>) -> ScopeVerdict {
    ScopeVerdict::InScope {
        matched, target_id: Some(target.id.clone()), parents: target.parents.clone(),
    }
}

/// Includes target, copy source, destination, ancestors and fresh post-checks.
/// Errors are memoized too. Clearing the cache never restores requests.
#[derive(Default)]
pub(crate) struct Budget {
    requests: usize,
    memo: HashMap<String, Result<FileMeta, MetaError>>,
}

impl Budget {
    pub(crate) fn new() -> Self { Self::default() }
    pub(crate) fn requests(&self) -> usize { self.requests }

    /// Discard metadata from the previous phase without resetting the request cap.
    /// The executor must call this before the post-approval `check`, reusing the
    /// same Budget from the initial check so all phases share the 16 requests.
    pub(crate) fn begin_fresh_phase(&mut self) {
        self.memo.clear();
    }

    pub(crate) async fn get<S: MetadataSource + ?Sized>(
        &mut self, src: &mut S, id: &GoogleId,
    ) -> Result<FileMeta, ScopeVerdict> {
        let result = if let Some(cached) = self.memo.get(id.as_str()) {
            cached.clone()
        } else {
            if self.requests >= MAX_REQUESTS { return Err(ScopeVerdict::LimitExceeded); }
            self.requests += 1;
            let result = src.get(id).await.and_then(|mut meta| {
                validate_meta(&meta)?;
                if meta.id != id.as_str() { return Err(MetaError::Failed); }
                meta.name = duduclaw_core::truncate_chars(&meta.name, MAX_NAME_CHARS);
                Ok(meta)
            });
            self.memo.insert(id.as_str().to_owned(), result.clone());
            result
        };
        result.map_err(|error| match error {
            MetaError::NotVisible => denied(ScopeReason::ParentNotVisible),
            MetaError::Failed => ScopeVerdict::Failed,
        })
    }
}

fn shortcut(meta: &FileMeta) -> bool {
    meta.mime_type == SHORTCUT_MIME || meta.shortcut_target.is_some()
}

fn listed(folders: &[FolderEntry], id: &str) -> bool {
    folders.iter().any(|folder| folder.id == id)
}

fn listed_drive(folders: &[FolderEntry], id: &str) -> bool {
    folders.iter().any(|folder| folder.kind == FolderKind::SharedDrive && folder.id == id)
}

fn listed_root(folders: &[FolderEntry], meta: &FileMeta) -> bool {
    meta.drive_id.as_deref() == Some(meta.id.as_str()) && listed_drive(folders, &meta.id)
}

fn folder(meta: &FileMeta, folders: &[FolderEntry]) -> bool {
    !shortcut(meta) && (meta.mime_type == FOLDER_MIME || listed_root(folders, meta))
}

fn parents(meta: &FileMeta) -> Result<&[String], ScopeVerdict> {
    match meta.parents.as_deref() {
        None => Err(denied(ScopeReason::ParentNotVisible)),
        Some([]) => Err(denied(ScopeReason::FolderOutOfScope)),
        Some(parents) => Ok(parents),
    }
}

/// Target depth is zero; at most twelve parent edges may be followed.
/// Fetch even a listed ancestor before stopping, to detect trashed folders.
/// Only the target's driveId permits the shared-drive short circuit.
pub(crate) async fn in_scope<S: MetadataSource + ?Sized>(
    src: &mut S, budget: &mut Budget, policy_folders: &[FolderEntry], target: &FileMeta,
) -> ScopeVerdict {
    if validate_meta(target).is_err() { return ScopeVerdict::Failed; }
    if target.trashed { return denied(ScopeReason::Trashed); }
    if let Some(drive_id) = &target.drive_id {
        if listed_drive(policy_folders, drive_id) {
            return allowed(target, vec![drive_id.clone()]);
        }
    }
    if listed(policy_folders, &target.id) {
        return if folder(target, policy_folders) {
            allowed(target, vec![target.id.clone()])
        } else {
            denied(ScopeReason::FolderOutOfScope)
        };
    }
    let initial = match parents(target) {
        Ok(parents) => parents,
        Err(verdict) => return verdict,
    };
    let mut pending: Vec<_> = initial.iter().map(|id| (id.clone(), 1usize)).collect();
    let mut matched = BTreeSet::new();
    // Revisit a node only at a greater depth: shared DAGs stay bounded while
    // longer paths (including cycles) cannot evade the per-chain depth limit.
    let mut deepest: HashMap<String, usize> = HashMap::new();
    while let Some((id, depth)) = pending.pop() {
        if depth > MAX_DEPTH { return ScopeVerdict::LimitExceeded; }
        if deepest.get(&id).is_some_and(|previous| *previous >= depth) { continue; }
        deepest.insert(id.clone(), depth);
        let google_id = match GoogleId::new(id) {
            Ok(id) => id,
            Err(_) => return ScopeVerdict::Failed,
        };
        let meta = match budget.get(src, &google_id).await {
            Ok(meta) => meta,
            Err(verdict) => return verdict,
        };
        if meta.trashed { return denied(ScopeReason::Trashed); }
        if shortcut(&meta) { return denied(ScopeReason::ShortcutNotAllowed); }
        if !folder(&meta, policy_folders) { return denied(ScopeReason::FolderOutOfScope); }
        if listed(policy_folders, &meta.id) {
            matched.insert(meta.id);
            continue;
        }
        let next = match parents(&meta) {
            Ok(parents) => parents,
            Err(verdict) => return verdict,
        };
        pending.extend(next.iter().map(|id| (id.clone(), depth + 1)));
    }
    allowed(target, matched.into_iter().collect())
}

#[derive(Clone, Debug)]
pub(crate) enum Plan {
    Create { parent: Option<GoogleId> },
    Copy { source: GoogleId, parent: GoogleId },
    Modify { target: GoogleId, rename: bool },
    Move { target: GoogleId, destination: GoogleId },
    Trash { target: GoogleId },
}

async fn check_parent<S: MetadataSource + ?Sized>(
    src: &mut S, budget: &mut Budget, folders: &[FolderEntry], id: &GoogleId,
) -> ScopeVerdict {
    let meta = match budget.get(src, id).await {
        Ok(meta) => meta,
        Err(verdict) => return verdict,
    };
    if meta.trashed { return denied(ScopeReason::Trashed); }
    if shortcut(&meta) { return denied(ScopeReason::ShortcutNotAllowed); }
    if !folder(&meta, folders) { return denied(ScopeReason::FolderOutOfScope); }
    if !meta.capabilities.can_add_children { return denied(ScopeReason::CapabilityDenied); }
    if folders.is_empty() { return allowed(&meta, vec![]); }
    in_scope(src, budget, folders, &meta).await
}

/// Call only after the policy/action gate. No writes are performed here.
pub(crate) async fn check<S: MetadataSource + ?Sized>(
    src: &mut S, budget: &mut Budget, policy: &DriveWritePolicy, action: Plan,
) -> ScopeVerdict {
    let folders = &policy.folders;
    if folders.is_empty() && matches!(&action, Plan::Move { .. } | Plan::Trash { .. }) {
        return denied(ScopeReason::FolderRequired);
    }
    let (id, rename, destination, is_trash) = match action {
        Plan::Create { parent: None } => {
            return if folders.is_empty() {
                ScopeVerdict::InScope { matched: vec![], target_id: None, parents: None }
            } else {
                denied(ScopeReason::FolderRequired)
            };
        }
        Plan::Create { parent: Some(parent) } => {
            return check_parent(src, budget, folders, &parent).await;
        }
        Plan::Copy { source, parent } => {
            let source = match budget.get(src, &source).await {
                Ok(meta) => meta,
                Err(verdict) => return verdict,
            };
            if source.trashed { return denied(ScopeReason::Trashed); }
            return match check_parent(src, budget, folders, &parent).await {
                ScopeVerdict::InScope { matched, .. } => allowed(&source, matched),
                verdict => verdict,
            };
        }
        Plan::Modify { target, rename } => (target, rename, None, false),
        Plan::Move { target, destination } => (target, false, Some(destination), false),
        Plan::Trash { target } => (target, false, None, true),
    };
    let target = match budget.get(src, &id).await {
        Ok(meta) => meta,
        Err(verdict) => return verdict,
    };
    if target.trashed { return denied(ScopeReason::Trashed); }
    if (rename || destination.is_some() || is_trash) && listed(folders, &target.id) {
        return denied(ScopeReason::ListedFolderProtected);
    }
    let can_act = if destination.is_some() {
        // The executor needs actual nonempty parents for removeParents even
        // when the shared-drive shortcut establishes scope without a walk.
        if let Err(verdict) = parents(&target) { return verdict; }
        target.capabilities.can_move_item_within_drive
    } else if is_trash {
        target.capabilities.can_trash
    } else {
        if shortcut(&target) { return denied(ScopeReason::ShortcutNotAllowed); }
        target.capabilities.can_edit && (!rename || target.capabilities.can_rename)
    };
    if !can_act { return denied(ScopeReason::CapabilityDenied); }
    let mut matched = if folders.is_empty() {
        vec![]
    } else {
        match in_scope(src, budget, folders, &target).await {
            ScopeVerdict::InScope { matched, .. } => matched,
            verdict => return verdict,
        }
    };
    if let Some(destination) = destination {
        let dest = match budget.get(src, &destination).await {
            Ok(meta) => meta,
            Err(verdict) => return verdict,
        };
        if target.drive_id != dest.drive_id { return denied(ScopeReason::CrossDriveMove); }
        match check_parent(src, budget, folders, &destination).await {
            ScopeVerdict::InScope { matched: dest_matched, .. } => matched.extend(dest_matched),
            verdict => return verdict,
        }
    }
    matched.sort();
    matched.dedup();
    allowed(&target, matched)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostCheck { Ok, Violation, Incomplete }

pub(crate) fn post_check_parents(
    expected: &[String], response_parents: Option<&[String]>,
) -> PostCheck {
    let Some(actual) = response_parents else { return PostCheck::Incomplete; };
    let valid = |ids: &[String]| {
        ids.iter().all(|id| GoogleId::new(id.as_str()).is_ok())
            && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
    };
    if expected.is_empty() || !valid(expected) || !valid(actual) { return PostCheck::Incomplete; }
    if expected.iter().collect::<BTreeSet<_>>() == actual.iter().collect::<BTreeSet<_>>() {
        PostCheck::Ok
    } else {
        PostCheck::Violation
    }
}

/// Re-read ALL metadata under the original budget, including ancestors. The
/// target's trashed flag is expected after Trash; check ancestry, not whether
/// this already-completed action could be authorized a second time. Trashed
/// ancestors still fail. This helper never authorizes a new write.
pub(crate) async fn recheck<S: MetadataSource + ?Sized>(
    src: &mut S, budget: &mut Budget, policy: &DriveWritePolicy, target_id: &GoogleId,
) -> PostCheck {
    budget.begin_fresh_phase();
    let mut target = match budget.get(src, target_id).await {
        Ok(meta) => meta,
        Err(_) => return PostCheck::Incomplete,
    };
    if policy.folders.is_empty() { return PostCheck::Ok; }
    target.trashed = false;
    match in_scope(src, budget, &policy.folders, &target).await {
        ScopeVerdict::InScope { .. } => PostCheck::Ok,
        ScopeVerdict::OutOfScope { .. } => PostCheck::Violation,
        ScopeVerdict::LimitExceeded | ScopeVerdict::Failed => PostCheck::Incomplete,
    }
}

/// Canonical, domain-separated SHA-256. Length framing prevents delimiter
/// collisions; None differs from an empty list. Arguments/action/credentials
/// must also be bound by the approval layer: this digest binds scope only.
pub(crate) fn scope_digest(verdict: &ScopeVerdict) -> String {
    fn field(hash: &mut Sha256, value: &str) {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    fn ids(hash: &mut Sha256, values: &[String]) {
        let mut sorted = values.to_vec();
        sorted.sort();
        hash.update((sorted.len() as u64).to_be_bytes());
        for value in sorted { field(hash, &value); }
    }
    let mut hash = Sha256::new();
    field(&mut hash, "duduclaw:drive-write:scope:v1");
    match verdict {
        ScopeVerdict::InScope { matched, target_id, parents } => {
            field(&mut hash, "in_scope");
            ids(&mut hash, matched);
            hash.update([u8::from(target_id.is_some())]);
            if let Some(id) = target_id { field(&mut hash, id); }
            hash.update([u8::from(parents.is_some())]);
            if let Some(parents) = parents { ids(&mut hash, parents); }
        }
        ScopeVerdict::OutOfScope { reason } => {
            field(&mut hash, "out_of_scope");
            field(&mut hash, reason.code());
        }
        ScopeVerdict::LimitExceeded => field(&mut hash, "folder_check_limit"),
        ScopeVerdict::Failed => field(&mut hash, "failed"),
    }
    format!("sha256:{:x}", hash.finalize())
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
