use super::*;
use super::super::transport::{NotSentReason, SendError};
use serde_json::{json, Value};

const TARGET: &str = "target_00001";
const LISTED: &str = "listed_00001";
const SECOND: &str = "listed_00002";
const OUTSIDE: &str = "outside_001";
const PARENT: &str = "parent_00001";
const DEST: &str = "destination_01";
const DRIVE: &str = "shared_drive_01";
const ORIGINAL: &str = "original_001";

fn id(value: &str) -> GoogleId { GoogleId::new(value).unwrap() }
fn strings(values: &[&str]) -> Vec<String> { values.iter().map(|s| (*s).to_owned()).collect() }

fn meta(id: &str, parents: Option<&[&str]>) -> FileMeta {
    FileMeta {
        id: id.to_owned(), name: "資料夾🙂".to_owned(), mime_type: FOLDER_MIME.to_owned(),
        parents: parents.map(strings), drive_id: None, trashed: false, shortcut_target: None,
        capabilities: Caps {
            can_edit: true, can_rename: true, can_add_children: true,
            can_trash: true, can_move_item_within_drive: true,
        },
    }
}

fn policy(entries: &[(&str, FolderKind)]) -> DriveWritePolicy {
    let mut policy = DriveWritePolicy::all_off();
    policy.create = true;
    policy.modify = true;
    policy.move_ = true;
    policy.trash = true;
    policy.folders = entries.iter().map(|(id, kind)| FolderEntry {
        id: (*id).to_owned(), name: "資料夾🙂".to_owned(), kind: *kind,
    }).collect();
    policy
}

fn scoped_policy() -> DriveWritePolicy { policy(&[(LISTED, FolderKind::Folder)]) }

#[derive(Default)]
struct FakeSource {
    files: HashMap<String, Result<FileMeta, MetaError>>,
    requests: usize,
    seen: Vec<String>,
}

impl FakeSource {
    fn new(files: Vec<FileMeta>) -> Self {
        Self { files: files.into_iter().map(|meta| (meta.id.clone(), Ok(meta))).collect(),
            ..Self::default() }
    }

    fn put(&mut self, meta: FileMeta) { self.files.insert(meta.id.clone(), Ok(meta)); }
}

impl MetadataSource for FakeSource {
    fn get(&mut self, id: &GoogleId) -> BoxFuture<'_, Result<FileMeta, MetaError>> {
        self.requests += 1;
        self.seen.push(id.as_str().to_owned());
        let result = self.files.get(id.as_str()).cloned().unwrap_or(Err(MetaError::NotVisible));
        Box::pin(async move { result })
    }
}

fn assert_allowed(verdict: &ScopeVerdict) {
    assert!(matches!(verdict, ScopeVerdict::InScope { .. }), "{verdict:?}");
}

async fn run(src: &mut FakeSource, policy: &DriveWritePolicy, plan: Plan) -> ScopeVerdict {
    check(src, &mut Budget::new(), policy, plan).await
}

#[tokio::test]
async fn multiple_parents_one_outside_is_denied() {
    let target = meta(TARGET, Some(&[LISTED, OUTSIDE]));
    let mut src = FakeSource::new(vec![meta(LISTED, None), meta(OUTSIDE, Some(&[]))]);
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
        denied(ScopeReason::FolderOutOfScope));
}

#[tokio::test]
async fn multiple_parents_and_ancestor_branches_must_all_match() {
    let target = meta(TARGET, Some(&[PARENT, LISTED]));
    let policy = policy(&[(LISTED, FolderKind::Folder), (SECOND, FolderKind::Folder)]);
    let mut src = FakeSource::new(vec![meta(PARENT, Some(&[LISTED, SECOND])),
        meta(LISTED, None), meta(SECOND, None)]);
    let verdict = in_scope(&mut src, &mut Budget::new(), &policy.folders, &target).await;
    assert_eq!(verdict, allowed(&target, strings(&[LISTED, SECOND])));
    assert_eq!(src.requests, 3);
    src.put(meta(PARENT, Some(&[LISTED, OUTSIDE])));
    src.put(meta(OUTSIDE, Some(&[])));
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &policy.folders, &target).await,
        denied(ScopeReason::FolderOutOfScope));
}

#[tokio::test]
async fn missing_parents_and_invisible_ancestors_fail_closed() {
    let mut src = FakeSource::default();
    for target in [meta(TARGET, None), meta(TARGET, Some(&[PARENT]))] {
        assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
            denied(ScopeReason::ParentNotVisible));
    }
    src.put(meta(PARENT, None));
    let target = meta(TARGET, Some(&[PARENT]));
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
        denied(ScopeReason::ParentNotVisible));
    src.files.insert(PARENT.to_owned(), Err(MetaError::Failed));
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
        ScopeVerdict::Failed);
}

#[tokio::test]
async fn shared_drive_match_needs_no_ancestor_requests() {
    let mut target = meta(TARGET, None);
    target.drive_id = Some(DRIVE.to_owned());
    let mut src = FakeSource::default();
    let policy = policy(&[(DRIVE, FolderKind::SharedDrive)]);
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &policy.folders, &target).await,
        allowed(&target, strings(&[DRIVE])));
    assert_eq!(src.requests, 0);
    let wrong_kind = policy_for_drive_as_folder();
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &wrong_kind.folders, &target).await,
        denied(ScopeReason::ParentNotVisible));
}

fn policy_for_drive_as_folder() -> DriveWritePolicy { policy(&[(DRIVE, FolderKind::Folder)]) }

#[tokio::test]
async fn depth_twelve_allowed_thirteen_exceeded() {
    for depth in [12, 13] {
        let chain: Vec<_> = (1..=depth).map(|n| format!("ancestor_{n:03}")).collect();
        let mut src = FakeSource::default();
        for (index, node) in chain.iter().enumerate() {
            let next = chain.get(index + 1).map(|id| vec![id.as_str()]);
            src.put(meta(node, next.as_deref()));
        }
        let target = meta(TARGET, Some(&[chain[0].as_str()]));
        let policy = policy(&[(chain.last().unwrap().as_str(), FolderKind::Folder)]);
        let verdict = in_scope(&mut src, &mut Budget::new(), &policy.folders, &target).await;
        if depth == 12 { assert_allowed(&verdict); }
        else { assert_eq!(verdict, ScopeVerdict::LimitExceeded); }
        assert_eq!(src.requests, 12);
    }
}

#[tokio::test]
async fn seventeen_distinct_lookups_exceed_shared_budget() {
    let nodes: Vec<_> = (0..17).map(|n| format!("ancestor_{n:03}")).collect();
    let entries: Vec<_> = nodes.iter().map(|id| (id.as_str(), FolderKind::Folder)).collect();
    let parents: Vec<_> = nodes.iter().map(String::as_str).collect();
    let target = meta(TARGET, Some(&parents));
    let mut src = FakeSource::new(nodes.iter().map(|id| meta(id, None)).collect());
    let mut budget = Budget::new();
    let verdict = in_scope(&mut src, &mut budget, &policy(&entries).folders, &target).await;
    assert_eq!(verdict, ScopeVerdict::LimitExceeded);
    assert_eq!(verdict.reason_code(), Some("folder_check_limit"));
    assert_eq!(budget.requests(), 16);
    assert_eq!(src.requests, 16);
}

#[tokio::test]
async fn target_and_copy_destination_share_the_metadata_budget() {
    let mut src = FakeSource::new(vec![meta(TARGET, None), meta(LISTED, None)]);
    let mut budget = Budget::new();
    for n in 0..15 {
        let node = format!("prefetch_{n:03}");
        src.put(meta(&node, None));
        budget.get(&mut src, &id(&node)).await.unwrap();
    }
    assert_eq!(check(&mut src, &mut budget, &scoped_policy(), Plan::Copy {
        source: id(TARGET), parent: id(LISTED),
    }).await, ScopeVerdict::LimitExceeded);
    assert_eq!(src.requests, 16);
}

#[tokio::test]
async fn memo_hits_and_cached_errors_do_not_consume_budget() {
    let mut src = FakeSource::new(vec![meta(LISTED, None)]);
    let mut budget = Budget::new();
    // Also exercise the trait object and &mut MetadataSource implementations.
    let mut source: &mut dyn MetadataSource = &mut src;
    for _ in 0..20 {
        budget.get(&mut source, &id(LISTED)).await.unwrap();
        assert_eq!(budget.get(&mut source, &id(OUTSIDE)).await,
            Err(denied(ScopeReason::ParentNotVisible)));
    }
    assert_eq!(budget.requests(), 2);
    assert_eq!(src.requests, 2);
}

#[tokio::test]
async fn cycles_and_longer_dag_paths_cannot_bypass_depth_limit() {
    let target = meta(TARGET, Some(&[PARENT]));
    let mut src = FakeSource::new(vec![meta(PARENT, Some(&[PARENT]))]);
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
        ScopeVerdict::LimitExceeded);
    assert_eq!(src.requests, 1);

    // Visit LISTED first at depth one, then again at depth thirteen.
    let chain: Vec<_> = (0..12).map(|n| format!("ancestor_{n:03}")).collect();
    for (index, node) in chain.iter().enumerate() {
        let next = chain.get(index + 1).map(String::as_str).unwrap_or(LISTED);
        src.put(meta(node, Some(&[next])));
    }
    src.put(meta(LISTED, None));
    let target = meta(TARGET, Some(&[chain[0].as_str(), LISTED]));
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
        ScopeVerdict::LimitExceeded);
}

#[tokio::test]
async fn trashed_target_and_visited_ancestors_are_denied() {
    for trashed_id in [TARGET, PARENT, LISTED] {
        let mut target = meta(TARGET, Some(&[PARENT]));
        let mut parent = meta(PARENT, Some(&[LISTED]));
        let mut listed = meta(LISTED, None);
        for item in [&mut target, &mut parent, &mut listed] {
            item.trashed = item.id == trashed_id;
        }
        let mut src = FakeSource::new(vec![parent, listed]);
        assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
            denied(ScopeReason::Trashed));
    }
}

#[tokio::test]
async fn shortcuts_modify_denied_move_and_trash_bind_the_shortcut() {
    let mut target = meta(TARGET, Some(&[LISTED]));
    target.mime_type = SHORTCUT_MIME.to_owned();
    target.shortcut_target = Some(ORIGINAL.to_owned());
    let mut src = FakeSource::new(vec![target.clone(), meta(LISTED, None), meta(DEST, Some(&[LISTED]))]);
    assert_eq!(run(&mut src, &scoped_policy(), Plan::Modify { target: id(TARGET), rename: false }).await,
        denied(ScopeReason::ShortcutNotAllowed));
    assert_eq!(ScopeReason::ShortcutNotAllowed.user_text(), "這是捷徑，請改用原始檔案");
    for plan in [Plan::Trash { target: id(TARGET) },
        Plan::Move { target: id(TARGET), destination: id(DEST) }]
    {
        assert_eq!(run(&mut src, &scoped_policy(), plan).await, allowed(&target, strings(&[LISTED])));
    }
    assert!(!src.seen.iter().any(|id| id == ORIGINAL));
    target.parents = Some(strings(&[OUTSIDE]));
    src.put(target);
    src.put(meta(OUTSIDE, Some(&[])));
    for plan in [Plan::Trash { target: id(TARGET) },
        Plan::Move { target: id(TARGET), destination: id(DEST) }]
    {
        assert_eq!(run(&mut src, &scoped_policy(), plan).await, denied(ScopeReason::FolderOutOfScope));
    }
}

#[tokio::test]
async fn listed_folder_protected_except_creation_and_nonrename_modify() {
    let mut src = FakeSource::new(vec![meta(LISTED, None)]);
    for plan in [Plan::Modify { target: id(LISTED), rename: true },
        Plan::Move { target: id(LISTED), destination: id(DEST) },
        Plan::Trash { target: id(LISTED) }]
    {
        assert_eq!(run(&mut src, &scoped_policy(), plan).await,
            denied(ScopeReason::ListedFolderProtected));
    }
    for plan in [Plan::Create { parent: Some(id(LISTED)) },
        Plan::Modify { target: id(LISTED), rename: false }]
    {
        assert_allowed(&run(&mut src, &scoped_policy(), plan).await);
    }
}

#[tokio::test]
async fn move_cross_drive_and_outside_destination_are_denied() {
    let mut src = FakeSource::new(vec![meta(TARGET, Some(&[LISTED])), meta(LISTED, None)]);
    let both = policy(&[(LISTED, FolderKind::Folder), (DRIVE, FolderKind::SharedDrive)]);
    for (source_drive, dest_drive) in [
        (None, Some(DRIVE)), (Some(DRIVE), None), (Some(DRIVE), Some(SECOND)),
    ] {
        let mut target = meta(TARGET, Some(&[LISTED]));
        target.drive_id = source_drive.map(str::to_owned);
        let mut dest = meta(DEST, Some(&[LISTED]));
        dest.drive_id = dest_drive.map(str::to_owned);
        src.put(target);
        src.put(dest);
        assert_eq!(run(&mut src, &both, Plan::Move { target: id(TARGET), destination: id(DEST) }).await,
            denied(ScopeReason::CrossDriveMove));
    }
    src.put(meta(TARGET, Some(&[LISTED])));
    src.put(meta(DEST, Some(&[])));
    assert_eq!(run(&mut src, &both, Plan::Move { target: id(TARGET), destination: id(DEST) }).await,
        denied(ScopeReason::FolderOutOfScope));
}

#[tokio::test]
async fn move_within_drive_returns_all_google_parents() {
    for drive in [None, Some(DRIVE)] {
        let mut target = meta(TARGET, Some(&[LISTED, SECOND]));
        target.drive_id = drive.map(str::to_owned);
        let mut dest = meta(DEST, Some(&[LISTED]));
        dest.drive_id = target.drive_id.clone();
        let mut src = FakeSource::new(vec![target.clone(), dest, meta(LISTED, None), meta(SECOND, None)]);
        let policy = policy(&[(LISTED, FolderKind::Folder), (SECOND, FolderKind::Folder)]);
        assert_eq!(run(&mut src, &policy, Plan::Move { target: id(TARGET), destination: id(DEST) }).await,
            allowed(&target, strings(&[LISTED, SECOND])));
    }
}

#[tokio::test]
async fn each_action_checks_its_capability_before_walking() {
    let plans = [Plan::Create { parent: Some(id(TARGET)) },
        Plan::Modify { target: id(TARGET), rename: false },
        Plan::Modify { target: id(TARGET), rename: true },
        Plan::Move { target: id(TARGET), destination: id(DEST) },
        Plan::Trash { target: id(TARGET) }];
    for (index, plan) in plans.into_iter().enumerate() {
        let mut target = meta(TARGET, Some(&[LISTED]));
        match index {
            0 => target.capabilities.can_add_children = false,
            1 => target.capabilities.can_edit = false,
            2 => target.capabilities.can_rename = false,
            3 => target.capabilities.can_move_item_within_drive = false,
            4 => target.capabilities.can_trash = false,
            _ => unreachable!(),
        }
        let mut src = FakeSource::new(vec![target]);
        assert_eq!(run(&mut src, &scoped_policy(), plan).await, denied(ScopeReason::CapabilityDenied));
        assert_eq!(src.requests, 1);
    }
}

#[tokio::test]
async fn destination_requires_add_children_capability() {
    let mut dest = meta(DEST, Some(&[LISTED]));
    dest.capabilities.can_add_children = false;
    let mut src = FakeSource::new(vec![meta(TARGET, Some(&[LISTED])), meta(LISTED, None), dest]);
    for plan in [Plan::Move { target: id(TARGET), destination: id(DEST) },
        Plan::Copy { source: id(TARGET), parent: id(DEST) }]
    {
        assert_eq!(run(&mut src, &scoped_policy(), plan).await, denied(ScopeReason::CapabilityDenied));
    }
}

#[tokio::test]
async fn empty_list_skips_walk_but_requires_folder_for_move_and_trash() {
    let empty = policy(&[]);
    let mut src = FakeSource::new(vec![meta(TARGET, None)]);
    assert_allowed(&run(&mut src, &empty, Plan::Create { parent: None }).await);
    assert_eq!(src.requests, 0);
    for plan in [Plan::Create { parent: Some(id(TARGET)) },
        Plan::Modify { target: id(TARGET), rename: true },
        Plan::Copy { source: id(TARGET), parent: id(TARGET) }]
    {
        assert_allowed(&run(&mut src, &empty, plan).await);
    }
    assert_eq!(src.requests, 3);
    for plan in [Plan::Move { target: id(TARGET), destination: id(DEST) },
        Plan::Trash { target: id(TARGET) }]
    {
        assert_eq!(run(&mut src, &empty, plan).await, denied(ScopeReason::FolderRequired));
    }
    assert_eq!(src.requests, 3);
    assert_eq!(run(&mut src, &scoped_policy(), Plan::Create { parent: None }).await,
        denied(ScopeReason::FolderRequired));
}

#[tokio::test]
async fn empty_list_still_checks_shortcuts_trash_and_capabilities() {
    for (reason, target) in [
        (ScopeReason::Trashed, FileMeta { trashed: true, ..meta(TARGET, None) }),
        (ScopeReason::ShortcutNotAllowed, FileMeta { mime_type: SHORTCUT_MIME.to_owned(),
            shortcut_target: Some(ORIGINAL.to_owned()), ..meta(TARGET, None) }),
        (ScopeReason::CapabilityDenied, FileMeta { capabilities: Caps::default(), ..meta(TARGET, None) }),
    ] {
        let mut src = FakeSource::new(vec![target]);
        for plan in [Plan::Modify { target: id(TARGET), rename: false },
            Plan::Create { parent: Some(id(TARGET)) }]
        {
            assert_eq!(run(&mut src, &policy(&[]), plan).await, denied(reason));
        }
    }
}

#[tokio::test]
async fn copy_requires_visible_untrashed_source_but_not_source_scope() {
    let mut src = FakeSource::new(vec![meta(LISTED, None)]);
    let plan = Plan::Copy { source: id(TARGET), parent: id(LISTED) };
    assert_eq!(run(&mut src, &scoped_policy(), plan.clone()).await, denied(ScopeReason::ParentNotVisible));
    src.put(FileMeta { trashed: true, ..meta(TARGET, None) });
    assert_eq!(run(&mut src, &scoped_policy(), plan.clone()).await, denied(ScopeReason::Trashed));
    src.put(meta(TARGET, None));
    assert_allowed(&run(&mut src, &scoped_policy(), plan).await);
}

#[tokio::test]
async fn creation_requires_folder_or_listed_shared_drive_root() {
    let mut src = FakeSource::new(vec![FileMeta {
        mime_type: "text/plain".to_owned(), ..meta(TARGET, Some(&[LISTED]))
    }]);
    assert_eq!(run(&mut src, &scoped_policy(), Plan::Create { parent: Some(id(TARGET)) }).await,
        denied(ScopeReason::FolderOutOfScope));
    src.put(FileMeta { drive_id: Some(DRIVE.to_owned()), mime_type: "application/octet-stream".to_owned(),
        ..meta(DRIVE, None) });
    assert_allowed(&run(&mut src, &policy(&[(DRIVE, FolderKind::SharedDrive)]),
        Plan::Create { parent: Some(id(DRIVE)) }).await);
}

#[tokio::test]
async fn folder_names_never_grant_scope_and_unverified_ids_need_google_folders() {
    let mut src = FakeSource::new(vec![meta(OUTSIDE, Some(&[]))]);
    let target = meta(TARGET, Some(&[OUTSIDE]));
    assert_eq!(in_scope(&mut src, &mut Budget::new(), &scoped_policy().folders, &target).await,
        denied(ScopeReason::FolderOutOfScope));
    let unverified = policy(&[(LISTED, FolderKind::Unverified)]);
    src.put(meta(LISTED, None));
    assert_allowed(&run(&mut src, &unverified, Plan::Create { parent: Some(id(LISTED)) }).await);
    src.put(FileMeta { mime_type: SHORTCUT_MIME.to_owned(), shortcut_target: Some(ORIGINAL.to_owned()),
        ..meta(LISTED, None) });
    assert_eq!(run(&mut src, &unverified, Plan::Create { parent: Some(id(LISTED)) }).await,
        denied(ScopeReason::ShortcutNotAllowed));
}

#[test]
fn post_check_parents_compares_sets_and_reports_missing_or_wrong_parents() {
    let expected = strings(&[LISTED, SECOND]);
    assert_eq!(post_check_parents(&expected, Some(&strings(&[SECOND, LISTED]))), PostCheck::Ok);
    assert_eq!(post_check_parents(&expected, Some(&strings(&[LISTED, OUTSIDE]))), PostCheck::Violation);
    assert_eq!(post_check_parents(&expected, Some(&[])), PostCheck::Violation);
    assert_eq!(post_check_parents(&expected, None), PostCheck::Incomplete);
    assert_eq!(post_check_parents(&[], Some(&[])), PostCheck::Incomplete);
    assert_eq!(post_check_parents(&expected, Some(&strings(&["bad/id"]))), PostCheck::Incomplete);
    assert_eq!(post_check_parents(&expected, Some(&strings(&[LISTED, LISTED]))), PostCheck::Incomplete);
}

#[tokio::test]
async fn fresh_phase_rechecks_moved_target_destination_and_ancestor() {
    for moved in [TARGET, DEST, PARENT] {
        let mut src = FakeSource::new(vec![meta(TARGET, Some(&[PARENT])),
            meta(PARENT, Some(&[LISTED])), meta(DEST, Some(&[LISTED])),
            meta(LISTED, None), meta(OUTSIDE, Some(&[]))]);
        let mut budget = Budget::new();
        let plan = Plan::Move { target: id(TARGET), destination: id(DEST) };
        assert_allowed(&check(&mut src, &mut budget, &scoped_policy(), plan.clone()).await);
        let initial_requests = budget.requests();
        assert_eq!(initial_requests, 4);
        // A move while the approval dialog is open invalidates the old memo.
        src.put(meta(moved, Some(&[OUTSIDE])));
        budget.begin_fresh_phase();
        assert_eq!(budget.requests(), initial_requests);
        assert_eq!(check(&mut src, &mut budget, &scoped_policy(), plan).await,
            denied(ScopeReason::FolderOutOfScope), "moved {moved}");
        assert!(budget.requests() > initial_requests);
        assert_eq!(budget.requests(), src.requests);
    }
}

#[tokio::test]
async fn fresh_phases_share_sixteen_request_folder_check_limit() {
    let chain: Vec<String> = (0..7).map(|n| format!("ancestor_{n:03}")).collect();
    let mut src = FakeSource::new(vec![meta(TARGET, Some(&[chain[0].as_str()])), meta(LISTED, None)]);
    for (index, node) in chain.iter().enumerate() {
        let parent = chain.get(index + 1).map(String::as_str).unwrap_or(LISTED);
        src.put(meta(node, Some(&[parent])));
    }
    let plan = Plan::Modify { target: id(TARGET), rename: false };
    let mut budget = Budget::new();
    assert_allowed(&check(&mut src, &mut budget, &scoped_policy(), plan.clone()).await);
    assert_eq!(budget.requests(), 9);
    budget.begin_fresh_phase();
    assert_eq!(budget.requests(), 9);
    let verdict = check(&mut src, &mut budget, &scoped_policy(), plan.clone()).await;
    assert_eq!(verdict, ScopeVerdict::LimitExceeded);
    assert_eq!(verdict.reason_code(), Some("folder_check_limit"));
    assert_eq!(budget.requests(), 16);
    assert_eq!(src.requests, 16);
    budget.begin_fresh_phase();
    assert_eq!(check(&mut src, &mut budget, &scoped_policy(), plan).await, ScopeVerdict::LimitExceeded);
    assert_eq!(src.requests, 16);
}

#[tokio::test]
async fn recheck_refreshes_target_and_ancestor_cache_and_detects_violation() {
    for change_target in [true, false] {
        let mut src = FakeSource::new(vec![meta(TARGET, Some(&[PARENT])),
            meta(PARENT, Some(&[LISTED])), meta(LISTED, None), meta(OUTSIDE, Some(&[]))]);
        let mut budget = Budget::new();
        assert_allowed(&check(&mut src, &mut budget, &scoped_policy(),
            Plan::Modify { target: id(TARGET), rename: false }).await);
        assert_eq!(budget.requests(), 3);
        src.put(meta(if change_target { TARGET } else { PARENT }, Some(&[OUTSIDE])));
        assert_eq!(recheck(&mut src, &mut budget, &scoped_policy(), &id(TARGET)).await, PostCheck::Violation);
        assert_eq!(budget.requests(), if change_target { 5 } else { 6 });
        assert_eq!(budget.requests(), src.requests);
    }
}

#[tokio::test]
async fn recheck_budget_exhaustion_and_fetch_failure_are_incomplete() {
    let mut src = FakeSource::new(vec![meta(TARGET, Some(&[LISTED])), meta(LISTED, None)]);
    let mut budget = Budget::new();
    for n in 0..15 {
        let node = format!("prefetch_{n:03}");
        src.put(meta(&node, None));
        budget.get(&mut src, &id(&node)).await.unwrap();
    }
    assert_eq!(recheck(&mut src, &mut budget, &scoped_policy(), &id(TARGET)).await, PostCheck::Incomplete);
    assert_eq!(src.requests, 16);
    assert_eq!(recheck(&mut src, &mut budget, &scoped_policy(), &id(TARGET)).await, PostCheck::Incomplete);
    assert_eq!(src.requests, 16);
    src.files.insert(TARGET.to_owned(), Err(MetaError::Failed));
    assert_eq!(recheck(&mut src, &mut Budget::new(), &scoped_policy(), &id(TARGET)).await, PostCheck::Incomplete);
}

#[tokio::test]
async fn recheck_after_trash_checks_ancestry_without_reauthorizing_the_target() {
    let mut src = FakeSource::new(vec![FileMeta { trashed: true, ..meta(TARGET, Some(&[LISTED])) },
        meta(LISTED, None)]);
    assert_eq!(recheck(&mut src, &mut Budget::new(), &scoped_policy(), &id(TARGET)).await, PostCheck::Ok);
    src.put(FileMeta { trashed: true, ..meta(LISTED, None) });
    assert_eq!(recheck(&mut src, &mut Budget::new(), &scoped_policy(), &id(TARGET)).await, PostCheck::Violation);
}

fn wire() -> Value {
    json!({"id": TARGET, "name": "資料夾🙂", "mimeType": FOLDER_MIME,
        "parents": [LISTED], "trashed": false, "capabilities": {"canEdit": true}})
}

fn parse(value: Value) -> Result<FileMeta, MetaError> {
    metadata_result(WriteResult::Ok((200, serde_json::to_vec(&value).unwrap())), &id(TARGET))
}

#[test]
fn metadata_http_errors_and_transport_uncertainty_fail_closed() {
    for status in [403, 404] {
        assert_eq!(metadata_result(WriteResult::Ok((status, vec![])), &id(TARGET)), Err(MetaError::NotVisible));
    }
    for result in [WriteResult::Ok((302, vec![])), WriteResult::Ok((429, vec![])),
        WriteResult::Ok((500, vec![])), WriteResult::Ok((200, b"not json".to_vec())),
        WriteResult::Ok((204, vec![])), WriteResult::NotSent(NotSentReason::RequestLimit),
        WriteResult::MaybeSent(SendError::Network)]
    {
        assert_eq!(metadata_result(result, &id(TARGET)), Err(MetaError::Failed));
    }
}

#[test]
fn metadata_is_strict_and_missing_capabilities_default_false() {
    let parsed = parse(wire()).unwrap();
    assert!(parsed.capabilities.can_edit);
    assert!(!parsed.capabilities.can_rename);
    assert!(!parsed.capabilities.can_add_children);
    assert!(!parsed.capabilities.can_trash);
    assert!(!parsed.capabilities.can_move_item_within_drive);
    let mut missing = wire();
    missing.as_object_mut().unwrap().remove("capabilities");
    assert_eq!(parse(missing).unwrap().capabilities, Caps::default());
    for field in ["id", "name", "mimeType", "trashed"] {
        let mut value = wire();
        value.as_object_mut().unwrap().remove(field);
        assert_eq!(parse(value), Err(MetaError::Failed), "missing {field}");
    }
    for (field, value) in [
        ("id", json!(OUTSIDE)), ("id", json!("../invalid")),
        ("name", json!(3)), ("mimeType", json!("")), ("trashed", json!("false")),
        ("parents", Value::Null), ("parents", json!([LISTED, 4])),
        ("parents", json!([LISTED, "bad/id"])), ("parents", json!([LISTED, LISTED])),
        ("driveId", Value::Null), ("driveId", json!(7)),
        ("shortcutDetails", Value::Null), ("shortcutDetails", json!({})),
        ("capabilities", Value::Null), ("capabilities", json!({"canEdit": "true"})),
        ("capabilities", json!({"canRename": null})),
    ] {
        let mut invalid = wire();
        invalid[field] = value;
        assert_eq!(parse(invalid), Err(MetaError::Failed), "invalid {field}");
    }
    let mut missing_parents = wire();
    missing_parents.as_object_mut().unwrap().remove("parents");
    assert_eq!(parse(missing_parents).unwrap().parents, None);
    let mut name = wire();
    name["name"] = json!("中🙂".repeat(300));
    assert_eq!(parse(name).unwrap().name, "中🙂".repeat(MAX_NAME_CHARS / 2));
}

#[tokio::test]
async fn source_returning_wrong_identity_or_malformed_parent_fails() {
    let mut src = FakeSource::default();
    for response in [meta(OUTSIDE, Some(&[LISTED])), meta(TARGET, Some(&["bad/id"]))] {
        src.files.insert(TARGET.to_owned(), Ok(response));
        assert_eq!(run(&mut src, &policy(&[]), Plan::Modify { target: id(TARGET), rename: false }).await,
            ScopeVerdict::Failed);
    }
}

#[test]
fn digest_is_order_stable_and_binds_ids_parents_and_verdict() {
    let target = meta(TARGET, Some(&[LISTED, SECOND]));
    let verdict = allowed(&target, strings(&[LISTED, SECOND]));
    let digest = scope_digest(&verdict);
    let reordered = allowed(&meta(TARGET, Some(&[SECOND, LISTED])), strings(&[SECOND, LISTED]));
    assert_eq!(digest, scope_digest(&reordered));
    for changed in [allowed(&meta(OUTSIDE, Some(&[LISTED, SECOND])), strings(&[LISTED, SECOND])),
        allowed(&meta(TARGET, Some(&[OUTSIDE])), strings(&[LISTED, SECOND])),
        allowed(&target, strings(&[LISTED])), allowed(&meta(TARGET, None), strings(&[LISTED, SECOND])),
        denied(ScopeReason::FolderOutOfScope), ScopeVerdict::Failed, ScopeVerdict::LimitExceeded]
    {
        assert_ne!(digest, scope_digest(&changed));
    }
    assert_ne!(scope_digest(&allowed(&meta(TARGET, None), vec![])),
        scope_digest(&allowed(&meta(TARGET, Some(&[])), vec![])));
    assert_eq!(digest.len(), "sha256:".len() + 64);
}
