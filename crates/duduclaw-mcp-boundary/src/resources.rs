use std::{collections::BTreeMap, fs::File, path::{Component, Path}};
use duduclaw_isolation::{capability::{CapabilityRecord, EmployeeBinding, PublicOrigin, ResourceGrant, RequestContext,
    VerificationError}, ids::EmployeeId};

/// Reject malformed resource policy before it can be signed.
pub(crate) fn validate_grants(record: &CapabilityRecord) -> Result<(), VerificationError> {
    record.validate_resource_mode().map_err(|_| VerificationError::Invalid)?;
    for grant in record.io_resources() {
        crate::tool_policy::validate_resource(grant)?;
        match grant {
            ResourceGrant::Workspace { handle } if handle.as_str().is_empty() => return Err(VerificationError::Invalid),
            ResourceGrant::Connector { binding: EmployeeBinding::Personal(owner), .. }
                if !record.personal_owner_matches(owner.as_str()) => return Err(VerificationError::Invalid),
            ResourceGrant::Connector { binding, .. } if binding != &record.employee_binding
                && !matches!((&record.employee_binding, binding), (EmployeeBinding::Personal(_), EmployeeBinding::Shared(_))) =>
                return Err(VerificationError::BindingMismatch),
            ResourceGrant::UrlOrigin { origin } => crate::tool_policy::validate_origin(origin.as_str())?,
            _ => {}
        }
    }
    Ok(())
}

pub fn require_resource(context: &RequestContext, resource: &ResourceGrant) -> Result<(), VerificationError> {
    if context.record().io_resources().any(|grant| grant == resource) { Ok(()) }
    else { Err(VerificationError::Invalid) }
}

/// D-H2: the exact public HTTPS origin a tool URL argument would contact, or a
/// refusal. Uses the same WHATWG parser as the fetcher, so the origin checked
/// is the origin dialled: https only, default port, no userinfo, a DNS name
/// (never an IP literal, including WHATWG numeric hosts) that passes
/// [`PublicOrigin::parse`]. Whether its addresses are public is decided by the
/// host fetch after DNS, per resolved address, and the answer is pinned.
pub fn public_url_origin(url: &str) -> Result<PublicOrigin, VerificationError> {
    if url.is_empty() || url.len() > 8192 || url.trim() != url || url.chars().any(char::is_control) {
        return Err(VerificationError::Invalid);
    }
    let parsed = url::Url::parse(url).map_err(|_| VerificationError::Invalid)?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some()
        || parsed.port().is_some() {
        return Err(VerificationError::Invalid);
    }
    let Some(url::Host::Domain(host)) = parsed.host() else { return Err(VerificationError::Invalid); };
    let origin = PublicOrigin::parse(&format!("https://{host}")).map_err(|_| VerificationError::Invalid)?;
    if parsed.origin().ascii_serialization() != origin.as_str() { return Err(VerificationError::Invalid); }
    Ok(origin)
}

/// The origin of `url` if the `PublicOrigins` grant permits it.
pub fn permitted_public_origin(grant: &ResourceGrant, url: &str) -> Result<PublicOrigin, VerificationError> {
    let ResourceGrant::PublicOrigins { origins } = grant else { return Err(VerificationError::Invalid); };
    let origin = public_url_origin(url)?;
    if origins.as_slice().iter().any(|allowed| allowed.permits(&origin)) { Ok(origin) }
    else { Err(VerificationError::Invalid) }
}

/// A URL argument resolved into an origin authorized by this call's signed
/// `PublicOrigins` grant. Fetch only [`AuthorizedPublicUrl::url`], through the
/// gateway's protected fetch (DNS pin, public addresses only, no redirects).
#[derive(Clone)]
pub struct AuthorizedPublicUrl {
    binding: CallBinding,
    grant: ResourceGrant,
    url: String,
    origin: PublicOrigin,
}
impl AuthorizedPublicUrl {
    pub fn url(&self) -> &str { &self.url }
    pub fn origin(&self) -> &PublicOrigin { &self.origin }
    /// The exact hostname to resolve; never `*`.
    pub fn host(&self) -> &str { self.origin.host().unwrap_or_default() }
    pub fn verify(&self, call: &crate::tool_policy::AuthorizedToolCall) -> Result<(), VerificationError> {
        if !self.binding.matches(call) || !call.resources().contains(&self.grant) {
            return Err(VerificationError::BindingMismatch);
        }
        Ok(())
    }
}

/// Resolve the `url` argument of an authorized web call through its signed
/// `PublicOrigins` grant. Refuses calls that were not authorized on that path.
pub fn authorize_public_url(call: &crate::tool_policy::AuthorizedToolCall)
    -> Result<AuthorizedPublicUrl, VerificationError> {
    if !crate::tool_policy::PUBLIC_ORIGIN_TOOLS.contains(&call.name()) {
        return Err(VerificationError::BindingMismatch);
    }
    let context = call.context();
    let scope = ResourceGrant::EmployeeScope { id: context.record().employee };
    let grant = call.resources().iter().find(|grant| matches!(grant, ResourceGrant::PublicOrigins { .. }))
        .ok_or(VerificationError::BindingMismatch)?;
    if !call.resources().contains(&scope) { return Err(VerificationError::BindingMismatch); }
    require_resource(context, &scope)?;
    require_resource(context, grant)?;
    let url = call.arguments().get("url").and_then(serde_json::Value::as_str).ok_or(VerificationError::Invalid)?;
    let origin = permitted_public_origin(grant, url)?;
    context.verify_live(crate::authority::now_seconds()?)?;
    Ok(AuthorizedPublicUrl { binding: CallBinding::new(call), grant: grant.clone(), url: url.to_owned(), origin })
}

pub fn relative_path(path: &str) -> Result<&Path, VerificationError> {
    if path.is_empty() || path.len() > 4096 || path.bytes().any(|b| b == 0 || b == b'\\')
        || path.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(VerificationError::Invalid);
    }
    let path = Path::new(path);
    if path.components().any(|part| !matches!(part, Component::Normal(_))) {
        return Err(VerificationError::Invalid);
    }
    Ok(path)
}

/// Fixed handle of the brokered employee work root (D-H1). The employee work
/// directory is employee-uid 0700 and attached only during a run's storage
/// lease, so the gateway never holds a dirfd for it: root opens each file.
pub const EMPLOYEE_WORK_ROOT: &str = "employee-work";
/// Largest regular file a brokered open may return.
pub const BROKERED_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Work path bounds shared with the root IPC `OpenWorkFile` (H1) and the broker
/// core (H2). They must apply exactly these rules; see [`WorkRelativePath::parse`].
pub const WORK_PATH_MAX_BYTES: usize = 4096;
pub const WORK_PATH_MAX_COMPONENT_BYTES: usize = 255;
pub const WORK_PATH_MAX_DEPTH: usize = 64;

/// Shared conformance vectors for the work path rules. H1 (IPC decode) and H2
/// (broker) cannot depend on this crate and must pass the same vectors.
pub const WORK_PATH_CONFORMANCE: &[(&str, bool)] = &[
    ("a", true), ("notes/員工.md", true), ("a/b/c.csv", true), (".hidden", true), ("..a/b..", true),
    ("a b/c-d_e.txt", true),
    ("", false), (".", false), ("..", false), ("/", false), ("/etc/passwd", false), ("../x", false),
    ("a/../b", false), ("a/./b", false), ("./a", false), ("a/.", false), ("a//b", false), ("a/", false),
    ("a\\b", false), ("a\0b", false), ("a\nb", false), ("a\tb", false), ("a\u{7f}b", false), ("a\u{85}b", false),
];

/// A validated path relative to the employee work root. It is never rewritten
/// (no case, Unicode or separator normalization): either the exact argument
/// bytes satisfy the rules or the call is refused, so the argument-bound
/// selection and the path root opens are byte-identical.
///
/// Rules (identical in H1/H2): 1..=4096 bytes of UTF-8; no NUL, `\` or Unicode
/// control character; `/`-separated components, each non-empty, not `.` or
/// `..`, at most 255 bytes; at most 64 components; no leading or trailing `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRelativePath(String);
impl WorkRelativePath {
    pub fn parse(path: &str) -> Result<Self, VerificationError> {
        relative_path(path)?;
        if path.len() > WORK_PATH_MAX_BYTES || path.chars().any(char::is_control)
            || path.split('/').count() > WORK_PATH_MAX_DEPTH
            || path.split('/').any(|part| part.len() > WORK_PATH_MAX_COMPONENT_BYTES) {
            return Err(VerificationError::Invalid);
        }
        Ok(Self(path.to_owned()))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

/// What root reports about the descriptor it handed out: since H2 a sealed
/// memfd snapshot (tmpfs dev/inode, `nlink` 0). The boundary re-checks every
/// field it can against `fstat` of the received fd and never trusts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkFileMetadata {
    pub regular: bool,
    pub size: u64,
    pub device: u64,
    pub inode: u64,
    pub nlink: u64,
}

/// An owned read-only, sealed memfd snapshot of a file root opened under the
/// live run's attached work mount, plus its metadata.
pub struct BrokeredWorkFile {
    pub file: File,
    pub metadata: WorkFileMetadata,
}

/// Broker diagnostics. The boundary maps every variant to `Unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkBrokerError {
    /// The run is not live or its storage lease is not attached.
    NotLive,
    NotFound,
    /// Symlink, other mount, wrong owner/device, not a regular file, nlink > 1.
    Refused,
    TooLarge,
    /// Root IPC or the broker itself failed.
    Unavailable,
}
impl WorkBrokerError {
    /// Stable reason code for audit/telemetry (never carries the path).
    pub fn code(self) -> &'static str {
        match self {
            Self::NotLive => "work_run_not_live",
            Self::NotFound => "work_file_not_found",
            Self::Refused => "work_file_refused",
            Self::TooLarge => "work_file_too_large",
            Self::Unavailable => "workspace_broker_unavailable",
        }
    }
    /// File-tool message (zh-TW). Fixed text: never the path, a file name,
    /// a host location or root's diagnostic.
    pub fn user_message(self) -> &'static str {
        match self {
            Self::NotLive => "目前沒有進行中的工作階段可以讀取工作區檔案，已拒絕讀取。",
            Self::NotFound => "工作區內找不到這個檔案，請確認相對路徑後重試。",
            Self::Refused => "這個檔案不能透過受保護的工作區讀取（例如符號連結、硬連結、非一般檔案或其他掛載點），已拒絕讀取。",
            Self::TooLarge => "檔案超過 16 MiB 上限，已拒絕讀取。",
            Self::Unavailable => "工作區檔案服務目前無法使用，已拒絕讀取；請稍後重試或聯絡管理員。",
        }
    }
}

/// Why a file-tool open was refused: a boundary verification failure, or the
/// root broker's typed refusal (H5). Lets file tools (H11) show a precise
/// message while the boundary contract (`authorize_file`) stays
/// `VerificationError::Unavailable` for every broker error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRefusal {
    Verification(VerificationError),
    Broker(WorkBrokerError),
}
impl From<VerificationError> for FileRefusal {
    fn from(error: VerificationError) -> Self { Self::Verification(error) }
}
impl FileRefusal {
    /// The boundary error: every broker refusal is `Unavailable`.
    pub fn verification(self) -> VerificationError {
        match self { Self::Verification(error) => error, Self::Broker(_) => VerificationError::Unavailable }
    }
    /// The root broker's refusal, if this came from root.
    pub fn broker(self) -> Option<WorkBrokerError> {
        match self { Self::Broker(error) => Some(error), Self::Verification(_) => None }
    }
}

/// One brokered open. Only this crate constructs it, and only from the verified
/// capability (run and employee), never from tool arguments.
pub struct WorkFileRequest<'a> {
    run: duduclaw_isolation::ids::RunId,
    employee: EmployeeId,
    path: &'a WorkRelativePath,
    max_bytes: u64,
}
impl WorkFileRequest<'_> {
    pub fn run(&self) -> duduclaw_isolation::ids::RunId { self.run }
    pub fn employee(&self) -> EmployeeId { self.employee }
    pub fn path(&self) -> &WorkRelativePath { self.path }
    pub fn max_bytes(&self) -> u64 { self.max_bytes }
}
#[cfg(any(test, feature = "test-support"))]
impl<'a> WorkFileRequest<'a> {
    pub fn for_test(run: duduclaw_isolation::ids::RunId, employee: EmployeeId, path: &'a WorkRelativePath) -> Self {
        Self { run, employee, path, max_bytes: BROKERED_FILE_MAX_BYTES }
    }
}

/// Root work broker (D-H1). Implemented by H5 over spawnd `OpenWorkFile`: root
/// opens `path` with openat2 BENEATH|NO_SYMLINKS|NO_MAGICLINKS|NO_XDEV under the
/// work mount of `run`, only while that run is live, owned by `employee` and
/// holds its storage lease; regular file, nlink 1, employee uid, at most
/// `max_bytes`; O_RDONLY|O_CLOEXEC|O_NOCTTY|O_NONBLOCK. Root then copies it
/// into a memfd sealed SEAL|SHRINK|GROW|WRITE and hands out that snapshot
/// reopened O_RDONLY (H2), so the claim reports the memfd honestly: tmpfs
/// device/inode, `nlink` 0. Blocking: call it from the blocking pool. An
/// implementation must never fall back to a host path.
pub trait WorkBroker: Send + Sync {
    fn open_work_file(&self, request: &WorkFileRequest<'_>) -> Result<BrokeredWorkFile, WorkBrokerError>;
}

/// Check the received descriptor itself. Only a sealed, read-only memfd
/// snapshot is accepted: regular, nlink 0, within the size limit, all four
/// seals (F_SEAL_WRITE|GROW|SHRINK|SEAL, so its bytes can never change), on
/// tmpfs, opened O_RDONLY, and the dev/inode/size root said it handed out.
/// Any live (unsealed) regular file — including the employee's own inode — is
/// refused: the same-run employee could rewrite or grow it after the check.
fn verify_brokered(opened: BrokeredWorkFile) -> Result<File, VerificationError> {
    let BrokeredWorkFile { file, metadata: claimed } = opened;
    if !claimed.regular || claimed.nlink != 0 || claimed.size > BROKERED_FILE_MAX_BYTES {
        return Err(VerificationError::Invalid);
    }
    let actual = file.metadata().map_err(|_| VerificationError::Unavailable)?;
    if !actual.is_file() || actual.len() > BROKERED_FILE_MAX_BYTES {
        return Err(VerificationError::Invalid);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        use rustix::fs::{fcntl_getfl, OFlags};
        if actual.nlink() != 0 { return Err(VerificationError::Invalid); }
        let flags = fcntl_getfl(&file).map_err(|_| VerificationError::Unavailable)?;
        if flags.intersects(OFlags::WRONLY | OFlags::RDWR | OFlags::PATH) {
            return Err(VerificationError::Invalid);
        }
        sealed_memfd(&file)?;
        if (actual.dev(), actual.ino(), actual.len()) != (claimed.device, claimed.inode, claimed.size) {
            return Err(VerificationError::Unavailable);
        }
        Ok(file)
    }
    // Sealed memfds are Linux-only; there is no brokered root elsewhere.
    #[cfg(not(target_os = "linux"))]
    { let _ = (file, actual); Err(VerificationError::Unavailable) }
}

/// The seals and filesystem of a memfd. `F_GET_SEALS` fails on non-shmem
/// files; an ordinary (or O_TMPFILE) tmpfs file starts with F_SEAL_SEAL only
/// and can never gain the others, so all four together mean a sealing memfd.
#[cfg(target_os = "linux")]
fn sealed_memfd(file: &File) -> Result<(), VerificationError> {
    use std::os::fd::AsRawFd;
    let raw = file.as_raw_fd();
    let required = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    // SAFETY: F_GET_SEALS only reads the borrowed live descriptor.
    #[allow(unsafe_code)]
    let seals = unsafe { libc::fcntl(raw, libc::F_GET_SEALS) };
    if seals < 0 || seals & required != required { return Err(VerificationError::Invalid); }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs writes one statfs into the provided storage, which is
    // read only after it reports success.
    #[allow(unsafe_code)]
    let filesystem = unsafe {
        if libc::fstatfs(raw, filesystem.as_mut_ptr()) != 0 { return Err(VerificationError::Unavailable); }
        filesystem.assume_init()
    };
    // f_type's width differs by libc/arch; the magic number fits 32 bits.
    if filesystem.f_type as u32 != libc::TMPFS_MAGIC as u32 { return Err(VerificationError::Invalid); }
    Ok(())
}

/// The workspace grant this call selected for `handle`: explicit selectors must
/// name it, and an implicit target is valid only when resolution chose one.
fn selected_workspace_grant<'c>(call: &'c crate::tool_policy::AuthorizedToolCall, handle: &str)
    -> Result<&'c ResourceGrant, VerificationError> {
    // A resolver may return several granted workspaces for a compound call.
    // The adapter cannot substitute one for the explicit execution target.
    let selectors = ["workspace", "workspace_id"].iter()
        .filter_map(|key| call.arguments().get(key))
        .map(|value| value.as_str().ok_or(VerificationError::Invalid))
        .collect::<Result<Vec<_>, _>>()?;
    if selectors.iter().any(|selected| *selected != handle) {
        return Err(VerificationError::BindingMismatch);
    }
    let workspaces = call.resources().iter().filter(|grant|
        matches!(grant, ResourceGrant::Workspace { .. })).collect::<Vec<_>>();
    if selectors.is_empty() && workspaces.len() != 1 {
        return Err(VerificationError::BindingMismatch);
    }
    workspaces.into_iter().find(|grant|
        matches!(grant, ResourceGrant::Workspace { handle: selected } if selected.as_str() == handle))
        .ok_or(VerificationError::BindingMismatch)
}

/// The selected input path must be explicitly present in this call.
fn require_selected_input(arguments: &serde_json::Value, relative: &str) -> Result<(), VerificationError> {
    let selected = ["path", "file_path", "input_path", "source_path"]
        .iter().filter_map(|key| arguments.get(key))
        .map(|value| value.as_str().ok_or(VerificationError::Invalid))
        .collect::<Result<Vec<_>, _>>()?;
    if selected.is_empty() || selected.iter().any(|selected| *selected != relative) {
        return Err(VerificationError::BindingMismatch);
    }
    Ok(())
}

/// A resource selection bound to one authorized call, including its arguments.
/// Private fields prevent an adapter from substituting a capability-wide grant.
#[derive(Clone)]
struct CallBinding {
    context: RequestContext,
    name: &'static str,
    arguments: serde_json::Value,
    resources: Vec<ResourceGrant>,
}
impl CallBinding {
    fn new(call: &crate::tool_policy::AuthorizedToolCall) -> Self {
        Self { context: call.context().clone(), name: call.name(),
            arguments: call.arguments().clone(), resources: call.resources().to_vec() }
    }
    fn matches(&self, call: &crate::tool_policy::AuthorizedToolCall) -> bool {
        self.context.record() == call.context().record() && self.name == call.name()
            && self.arguments == *call.arguments() && self.resources == call.resources()
    }
}

/// Selected workspace and its retained server-owned directory, never a pathname
/// or a grant chosen by a factory from the whole capability.
pub struct AuthorizedWorkspace {
    binding: CallBinding,
    grant: ResourceGrant,
    directory: File,
    identity: (u64, u64),
}
impl AuthorizedWorkspace {
    pub fn handle(&self) -> &str {
        match &self.grant { ResourceGrant::Workspace { handle } => handle.as_str(), _ => unreachable!() }
    }
    pub(crate) fn verify(&self, call: &crate::tool_policy::AuthorizedToolCall) -> Result<(), VerificationError> {
        if !self.binding.matches(call) || !call.resources().contains(&self.grant)
            || file_identity(&self.directory)? != self.identity {
            return Err(VerificationError::BindingMismatch);
        }
        Ok(())
    }

    /// The selected input path must be explicitly present in this call. Derived
    /// snapshots need their own reviewed adapter; arbitrary factory FDs cannot
    /// be promoted to authorized inputs.
    pub fn open_file(&self, relative: &str) -> Result<AuthorizedFile, VerificationError> {
        let path = relative_path(relative)?;
        require_selected_input(&self.binding.arguments, relative)?;
        self.binding.context.verify_live(crate::authority::now_seconds()?)?;
        let file = open_resource(&self.directory, path, false)?;
        AuthorizedFile::from_opened(self.binding.clone(), self.grant.clone(), file)
    }
}

impl duduclaw_isolation::execution::ExecutionAuthority for AuthorizedWorkspace {
    fn verify_live(&self) -> Result<(), duduclaw_isolation::execution::BackendError> {
        self.binding.context.verify_live(crate::authority::now_seconds()
            .map_err(|_| duduclaw_isolation::execution::BackendError::Denied)?)
            .map_err(|_| duduclaw_isolation::execution::BackendError::Denied)
    }
    fn verify_workspace(&self, handle: &str, directory: &File)
        -> Result<(), duduclaw_isolation::execution::BackendError> {
        use duduclaw_isolation::execution::BackendError;
        self.verify_live()?;
        if handle != self.handle() || !directory.metadata().map_err(|_| BackendError::Denied)?.is_dir()
            || file_identity(directory).map_err(|_| BackendError::Denied)? != self.identity
            || file_identity(&self.directory).map_err(|_| BackendError::Denied)? != self.identity {
            return Err(BackendError::Denied);
        }
        Ok(())
    }
}

/// A regular-file descriptor selected through a workspace dirfd or a reviewed
/// host snapshot source. Its selected grant, call binding and dev/inode travel
/// together through worker admission.
pub struct AuthorizedFile {
    binding: CallBinding,
    grant: ResourceGrant,
    file: File,
    identity: (u64, u64),
    invocation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    operation: Option<duduclaw_isolation::worker::ParseOperation>,
}
/// Reviewed gateway snapshots. A snapshot is bound to the exact call and its
/// selected source grant; arbitrary descriptors cannot acquire worker authority.
pub enum HostSnapshotSource<'a> {
    WebFetch { url: &'a str },
    InlineAudio,
    SynthesizedSpeech,
    Skill { agent: &'a str, name: &'a str },
}
impl AuthorizedFile {
    pub fn from_host_snapshot(call: &crate::tool_policy::AuthorizedToolCall,
        source: HostSnapshotSource<'_>, bytes: &[u8]) -> Result<Self, VerificationError> {
        use duduclaw_isolation::worker::ParseOperation;
        use std::io::{Seek, SeekFrom, Write};
        call.context().verify_live(crate::authority::now_seconds()?)?;
        let own = ResourceGrant::Employee { id: call.context().record().employee };
        // A D-H2 web call selects EmployeeScope + PublicOrigins, not Employee.
        let public = matches!(source, HostSnapshotSource::WebFetch { .. })
            && call.resources().iter().any(|grant| matches!(grant, ResourceGrant::PublicOrigins { .. }));
        if !public && !call.resources().contains(&own) { return Err(VerificationError::BindingMismatch); }
        let (grant, operation, cap) = match source {
            HostSnapshotSource::WebFetch { url } => {
                if call.name() != "web_extract" || call.arguments().get("url").and_then(serde_json::Value::as_str) != Some(url) {
                    return Err(VerificationError::BindingMismatch);
                }
                if public {
                    let authorized = authorize_public_url(call)?;
                    if authorized.url() != url { return Err(VerificationError::BindingMismatch); }
                    (authorized.grant, ParseOperation::ExtractHtml, 5 * 1024 * 1024)
                } else {
                    let parsed = url::Url::parse(url).map_err(|_| VerificationError::Invalid)?;
                    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none()
                        || !parsed.username().is_empty() || parsed.password().is_some() {
                        return Err(VerificationError::Invalid);
                    }
                    let origin = parsed.origin().ascii_serialization();
                    let grant = call.resources().iter().find(|r| matches!(r,
                        ResourceGrant::UrlOrigin { origin: selected } if selected.as_str() == origin))
                        .ok_or(VerificationError::BindingMismatch)?.clone();
                    (grant, ParseOperation::ExtractHtml, 5 * 1024 * 1024)
                }
            }
            HostSnapshotSource::InlineAudio => {
                if call.name() != "transcribe_audio"
                    || call.arguments().get("audio_base64").and_then(serde_json::Value::as_str).is_none() {
                    return Err(VerificationError::BindingMismatch);
                }
                (own, ParseOperation::InspectAudio, 25 * 1024 * 1024)
            }
            HostSnapshotSource::SynthesizedSpeech => {
                if call.name() != "synthesize_speech"
                    || call.arguments().get("text").and_then(serde_json::Value::as_str).is_none() {
                    return Err(VerificationError::BindingMismatch);
                }
                (own, ParseOperation::InspectAudio, 128 * 1024)
            }
            HostSnapshotSource::Skill { agent, name } => {
                let operation = match call.name() {
                    "skill_security_scan" => ParseOperation::ScanSkill,
                    "skill_extract" => ParseOperation::ExtractSkill,
                    "shared_skill_adopt" | "skill_graduate" => ParseOperation::PrepareSkill,
                    _ => return Err(VerificationError::BindingMismatch),
                };
                if call.arguments().get("skill_name").and_then(serde_json::Value::as_str) != Some(name)
                    || ["agent_id", "target_agent"].iter().filter_map(|key| call.arguments().get(key))
                        .any(|value| value.as_str() != Some(agent)) {
                    return Err(VerificationError::BindingMismatch);
                }
                let workspaces = call.resources().iter().filter(|r| matches!(r, ResourceGrant::Workspace { .. })).collect::<Vec<_>>();
                let [grant] = workspaces.as_slice() else { return Err(VerificationError::BindingMismatch); };
                ((*grant).clone(), operation, 64 * 1024)
            }
        };
        if bytes.len() > cap { return Err(VerificationError::Invalid); }
        let mut file = tempfile::tempfile().map_err(|_| VerificationError::Unavailable)?;
        file.write_all(bytes).map_err(|_| VerificationError::Unavailable)?;
        file.seek(SeekFrom::Start(0)).map_err(|_| VerificationError::Unavailable)?;
        let mut token = Self::from_opened(CallBinding::new(call), grant, file)?;
        token.operation = Some(operation);
        Ok(token)
    }
    pub(crate) fn verify_operation(&self, operation: duduclaw_isolation::worker::ParseOperation)
        -> Result<(), VerificationError> {
        if self.operation.is_some_and(|selected| selected != operation) {
            return Err(VerificationError::BindingMismatch);
        }
        Ok(())
    }
    fn from_opened(binding: CallBinding, grant: ResourceGrant, file: File) -> Result<Self, VerificationError> {
        if !file.metadata().map_err(|_| VerificationError::Unavailable)?.is_file() {
            return Err(VerificationError::Invalid);
        }
        let identity = file_identity(&file)?;
        Ok(Self { binding, grant, file, identity, invocation: None, operation: None })
    }
    pub fn metadata(&self) -> Result<std::fs::Metadata, VerificationError> {
        self.file.metadata().map_err(|_| VerificationError::Unavailable)
    }
    pub fn verify_live(&self) -> Result<(), VerificationError> {
        if self.invocation.as_ref().is_some_and(|cancelled|
            cancelled.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(VerificationError::Revoked);
        }
        self.binding.context.verify_live(crate::authority::now_seconds()?)
    }
    pub(crate) fn bind_invocation(&mut self, invocation: &InvocationGuard) -> Result<(), VerificationError> {
        if self.invocation.is_some() { return Err(VerificationError::BindingMismatch); }
        self.invocation = Some(invocation.cancelled.clone());
        self.verify_live()
    }
    pub(crate) fn verify(&self, call: &crate::tool_policy::AuthorizedToolCall) -> Result<(), VerificationError> {
        if !self.binding.matches(call) || !call.resources().contains(&self.grant)
            || file_identity(&self.file)? != self.identity {
            return Err(VerificationError::BindingMismatch);
        }
        Ok(())
    }
    pub(crate) fn file(&self) -> &File { &self.file }

    #[cfg(test)]
    pub(crate) fn for_test(call: &crate::tool_policy::AuthorizedToolCall, file: File) -> Self {
        let grant = call.resources().iter().find(|grant| matches!(grant, ResourceGrant::Workspace { .. })).unwrap();
        Self::from_opened(CallBinding::new(call), grant.clone(), file).unwrap()
    }
}

/// Async cancellation must also fence queued or already-running blocking work.
/// This marker belongs to one invocation, not the connection's reusable grant.
pub(crate) struct InvocationGuard {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl InvocationGuard {
    pub(crate) fn new() -> Self {
        Self { cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)) }
    }
}
impl Drop for InvocationGuard {
    fn drop(&mut self) { self.cancelled.store(true, std::sync::atomic::Ordering::Release); }
}

// The transport receives the retained descriptor without losing its authority.
// In particular a queued blocking snapshot must not keep reading after revoke.
impl std::io::Read for AuthorizedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.verify_live().map_err(|_| std::io::Error::new(
            std::io::ErrorKind::PermissionDenied, "isolated input authority unavailable"))?;
        std::io::Read::read(&mut self.file, buffer)
    }
}

fn file_identity(file: &File) -> Result<(u64, u64), VerificationError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().map_err(|_| VerificationError::Unavailable)?;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    { let _ = file; Err(VerificationError::Unavailable) }
}

/// An owned authorization proof for a helper's workspace. Only ResourceRoots
/// can construct it; no pathname, environment variable or MCP value can mint it.
/// Root reopens the fixed handle under H and verifies this directory identity.
pub struct AuthorizedHelperWorkspace {
    employee: EmployeeId,
    run: duduclaw_isolation::ids::RunId,
    policy: duduclaw_isolation::execution::WorkspacePolicy,
    _directory: File,
}
impl AuthorizedHelperWorkspace {
    /// Keep the proof bound to the caller while delivering its directory only
    /// for the separately reserved child run that root will admit.
    pub fn delivery_for(&self, run: duduclaw_isolation::execution::RunHandle)
        -> Result<duduclaw_isolation::execution::WorkspaceDelivery, VerificationError> {
        if run.0 == self.run { return Err(VerificationError::BindingMismatch); }
        Ok(duduclaw_isolation::execution::WorkspaceDelivery { employee: self.employee, run: run.0,
            policy: self.policy.clone(), directory: self._directory.try_clone().map_err(|_| VerificationError::Unavailable)? })
    }

    pub fn permits(&self, context: &RequestContext,
        policy: &duduclaw_isolation::execution::WorkspacePolicy) -> bool {
        self.employee == context.record().employee && self.run == context.record().run
            && self.policy == *policy && context.record().resources.as_slice().iter().any(|resource|
                matches!(resource, ResourceGrant::Workspace { handle } if handle == &policy.workspace))
    }
}

/// Server-owned roots, scoped to an employee. Register from trusted mount/dirfd
/// delivery only; never open a path supplied by an MCP client as a root.
#[derive(Default)]
pub struct ResourceRoots {
    roots: BTreeMap<String, (EmployeeId, File)>,
    wiki_roots: BTreeMap<String, (EmployeeId, File)>,
    /// The `employee-work` root: no dirfd, only root's broker (D-H1).
    brokered: Option<(EmployeeId, std::sync::Arc<dyn WorkBroker>)>,
}
impl ResourceRoots {
    /// File tools have exactly one explicit workspace selector. Resolve it here
    /// so the table adapter cannot manufacture an unrelated regular-file input.
    pub fn authorize_file(&self, context: &RequestContext, tool: &str, arguments: &serde_json::Value)
        -> Result<(crate::tool_policy::AuthorizedToolCall, AuthorizedFile), VerificationError> {
        self.authorize_file_with_refusal(context, tool, arguments).map_err(FileRefusal::verification)
    }

    /// [`Self::authorize_file`], keeping root's typed refusal so a file tool
    /// can answer with [`WorkBrokerError::user_message`] (H5/H11). Identical
    /// checks; only the error carries more detail.
    pub fn authorize_file_with_refusal(&self, context: &RequestContext, tool: &str, arguments: &serde_json::Value)
        -> Result<(crate::tool_policy::AuthorizedToolCall, AuthorizedFile), FileRefusal> {
        use crate::tool_policy::{self, ResourceResolver, Route, ToolPolicy};
        struct FileSelection;
        impl ResourceResolver for FileSelection {
            fn resolve(&self, _: &RequestContext, policy: &ToolPolicy, arguments: &serde_json::Value)
                -> Result<Vec<ResourceGrant>, VerificationError> {
                if policy.route != Route::File { return Err(VerificationError::Invalid); }
                let handle = arguments.get("workspace").and_then(serde_json::Value::as_str)
                    .ok_or(VerificationError::Invalid)?;
                Ok(vec![ResourceGrant::Workspace { handle: duduclaw_isolation::bounded::Text::new(handle.to_owned())
                    .map_err(|_| VerificationError::Invalid)? }])
            }
        }
        context.verify_live(crate::authority::now_seconds()?)?;
        let call = tool_policy::authorize(context, tool, arguments, &FileSelection)?;
        let handle = arguments.get("workspace").and_then(serde_json::Value::as_str).ok_or(VerificationError::Invalid)?;
        let path = arguments.get("path").and_then(serde_json::Value::as_str).ok_or(VerificationError::Invalid)?;
        // The brokered root has no gateway dirfd; it never reaches open_resource.
        let input = if self.is_brokered(handle) { self.open_brokered_file(&call, handle, path)? }
            else { self.authorize_workspace(&call, handle)?.open_file(path)? };
        Ok((call, input))
    }

    /// Register root's work broker as the `employee-work` root of `employee`.
    /// Exclusive with a dirfd root of the same handle, so there is never a
    /// host-path alternative to the broker.
    pub fn insert_brokered(&mut self, handle: String, employee: EmployeeId,
        broker: std::sync::Arc<dyn WorkBroker>) -> Result<(), VerificationError> {
        if handle != EMPLOYEE_WORK_ROOT || self.brokered.is_some() || self.roots.contains_key(&handle) {
            return Err(VerificationError::Invalid);
        }
        self.brokered = Some((employee, broker));
        Ok(())
    }

    pub fn is_brokered(&self, handle: &str) -> bool {
        handle == EMPLOYEE_WORK_ROOT && self.brokered.is_some()
    }

    /// Open `relative` in the brokered work root for this call's live run.
    /// Run and employee come only from the verified capability. A broker error
    /// is `Unavailable`; there is no fallback to opening a host path.
    pub fn authorize_brokered_file(&self, call: &crate::tool_policy::AuthorizedToolCall, handle: &str,
        relative: &str) -> Result<AuthorizedFile, VerificationError> {
        self.open_brokered_file(call, handle, relative).map_err(FileRefusal::verification)
    }

    fn open_brokered_file(&self, call: &crate::tool_policy::AuthorizedToolCall, handle: &str,
        relative: &str) -> Result<AuthorizedFile, FileRefusal> {
        let path = WorkRelativePath::parse(relative)?;
        let grant = selected_workspace_grant(call, handle)?.clone();
        let broker = self.brokered_root(call.context(), handle)?;
        require_selected_input(call.arguments(), relative)?;
        let context = call.context();
        context.verify_live(crate::authority::now_seconds()?)?;
        let record = context.record();
        let request = WorkFileRequest { run: record.run, employee: record.employee, path: &path,
            max_bytes: BROKERED_FILE_MAX_BYTES };
        let opened = broker.open_work_file(&request).map_err(FileRefusal::Broker)?;
        let file = verify_brokered(opened)?;
        // The run may have ended while root was opening the file.
        context.verify_live(crate::authority::now_seconds()?)?;
        Ok(AuthorizedFile::from_opened(CallBinding::new(call), grant, file)?)
    }

    fn brokered_root(&self, context: &RequestContext, handle: &str) -> Result<&dyn WorkBroker, VerificationError> {
        let (owner, broker) = self.brokered.as_ref().filter(|_| handle == EMPLOYEE_WORK_ROOT)
            .ok_or(VerificationError::Invalid)?;
        if owner != &context.record().employee || !context.record().io_resources()
            .any(|r| matches!(r, ResourceGrant::Workspace { handle: h } if h.as_str() == handle)) {
            return Err(VerificationError::Invalid);
        }
        Ok(broker.as_ref())
    }
    /// Mint authority only for a workspace selected by this call's resolver.
    pub fn authorize_workspace(&self, call: &crate::tool_policy::AuthorizedToolCall, handle: &str)
        -> Result<AuthorizedWorkspace, VerificationError> {
        let grant = selected_workspace_grant(call, handle)?;
        let root = self.authorized_root(call.context(), handle)?;
        let directory = root.try_clone().map_err(|_| VerificationError::Unavailable)?;
        let identity = file_identity(&directory)?;
        Ok(AuthorizedWorkspace { binding: CallBinding::new(call), grant: grant.clone(), directory, identity })
    }

    /// Gateway boot registers a trusted live wiki directory, never an
    /// employee-selected pathname. Wiki grants cannot open workspace roots.
    pub fn insert_wiki(&mut self, namespace: String, employee: EmployeeId, directory: File)
        -> Result<(), VerificationError> {
        if namespace.is_empty() || namespace.len() > 128 || namespace == "*"
            || self.wiki_roots.contains_key(&namespace) || self.wiki_roots.len() >= 128
            || !directory.metadata().map_err(|_| VerificationError::Unavailable)?.is_dir() {
            return Err(VerificationError::Invalid);
        }
        self.wiki_roots.insert(namespace, (employee, directory));
        Ok(())
    }

    /// Bind the retained dirfd to the live directory protected by the gateway's
    /// delivery fence. A stale publication snapshot must never serve withdrawn
    /// pages. `live` is opened from trusted gateway state, not MCP arguments.
    pub fn verify_wiki_root(&self, namespace: &str, live: &File) -> Result<(), VerificationError> {
        let (_, root) = self.wiki_roots.get(namespace).ok_or(VerificationError::Invalid)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let expected = live.metadata().map_err(|_| VerificationError::Unavailable)?;
            let actual = root.metadata().map_err(|_| VerificationError::Unavailable)?;
            if expected.is_dir() && expected.dev() == actual.dev() && expected.ino() == actual.ino() {
                return Ok(());
            }
        }
        #[cfg(not(unix))]
        { let _ = (root, live); }
        Err(VerificationError::BindingMismatch)
    }

    pub fn open_wiki_read(&self, context: &RequestContext, namespace: &str, relative: &str)
        -> Result<File, VerificationError> {
        let path = relative_path(relative)?;
        let (owner, root) = self.wiki_roots.get(namespace).ok_or(VerificationError::Invalid)?;
        if owner != &context.record().employee || !context.record().io_resources()
            .any(|grant| matches!(grant, ResourceGrant::Wiki { namespace: allowed } if allowed.as_str() == namespace)) {
            return Err(VerificationError::Invalid);
        }
        open_resource(root, path, false)
    }

    pub fn insert(&mut self, handle: String, employee: EmployeeId, directory: File) -> Result<(), VerificationError> {
        if handle.is_empty() || handle.len() > 128 || self.roots.contains_key(&handle)
            || self.is_brokered(&handle) || self.roots.len() >= 128 || !directory.metadata().map_err(|_| VerificationError::Unavailable)?.is_dir() {
            return Err(VerificationError::Invalid);
        }
        self.roots.insert(handle, (employee, directory));
        Ok(())
    }

    pub fn open_read(&self, context: &RequestContext, handle: &str, relative: &str)
        -> Result<File, VerificationError> {
        let path = relative_path(relative)?;
        let root = self.authorized_root(context, handle)?;
        open_resource(root, path, false)
    }

    /// Directory handles go only to an isolated A/E worker. Never convert this
    /// fd back to a host path or run a recursive gateway parser on its contents.
    pub fn open_directory(&self, context: &RequestContext, handle: &str, relative: Option<&str>)
        -> Result<File, VerificationError> {
        let path = match relative { Some(path) => relative_path(path)?, None => Path::new(".") };
        let root = self.authorized_root(context, handle)?;
        open_resource(root, path, true)
    }

    pub fn authorize_helper_workspace(&self, context: &RequestContext,
        policy: &duduclaw_isolation::execution::WorkspacePolicy) -> Result<AuthorizedHelperWorkspace, VerificationError> {
        let cwd = policy.relative_cwd.as_str();
        let directory = self.open_directory(context, policy.workspace.as_str(),
            if cwd.is_empty() || cwd == "." { None } else { Some(cwd) })?;
        Ok(AuthorizedHelperWorkspace { employee: context.record().employee, run: context.record().run,
            policy: policy.clone(), _directory: directory })
    }

    fn authorized_root(&self, context: &RequestContext, handle: &str) -> Result<&File, VerificationError> {
        let (owner, root) = self.roots.get(handle).ok_or(VerificationError::Invalid)?;
        if owner != &context.record().employee || !context.record().io_resources()
            .any(|r| matches!(r, ResourceGrant::Workspace { handle: h } if h.as_str() == handle)) {
            return Err(VerificationError::Invalid);
        }
        Ok(root)
    }
}

/// Test double for root's work broker. It emulates H2's checks on an ordinary
/// directory standing in for the live run's attached work mount, and H2's
/// sealed memfd snapshot. Production brokers are H5's
/// `work_bridge::SpawndWorkspaceBridge`; this double must never be wired at boot.
/// Linux only (sealed memfds), like the brokered root itself.
#[cfg(all(target_os = "linux", any(test, feature = "test-support")))]
pub mod test_broker {
    use super::{BrokeredWorkFile, WorkBroker, WorkBrokerError, WorkFileMetadata, WorkFileRequest};
    use duduclaw_isolation::ids::{EmployeeId, RunId};
    use std::{fs::File, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::MetadataExt},
        path::PathBuf, sync::Mutex};

    /// H2's snapshot: copy into a memfd, add SEAL|SHRINK|GROW|WRITE, reopen
    /// O_RDONLY through procfs (new description), drop the writable fd.
    pub fn sealed_snapshot(source: &mut File) -> std::io::Result<File> {
        // SAFETY: a NUL-terminated name; on success the kernel returns a new
        // descriptor that is owned exactly once below.
        #[allow(unsafe_code)]
        let raw = unsafe { libc::memfd_create(c"duduclaw-test-work".as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
        if raw < 0 { return Err(std::io::Error::last_os_error()); }
        #[allow(unsafe_code)]
        let mut writable = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        writable.write_all(&bytes)?;
        let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        // SAFETY: F_ADD_SEALS takes an int argument and touches no memory.
        #[allow(unsafe_code)]
        let sealed = unsafe { libc::fcntl(writable.as_raw_fd(), libc::F_ADD_SEALS, seals) };
        if sealed != 0 { return Err(std::io::Error::last_os_error()); }
        File::open(format!("/proc/self/fd/{}", writable.as_raw_fd()))
    }

    type Misreport = Box<dyn Fn(&mut WorkFileMetadata) + Send + Sync>;
    pub struct TestWorkBroker {
        root: PathBuf,
        live: Mutex<Option<(RunId, EmployeeId)>>,
        fail: Mutex<Option<WorkBrokerError>>,
        misreport: Mutex<Option<Misreport>>,
        calls: Mutex<Vec<(RunId, EmployeeId, String)>>,
        checked: bool,
    }
    impl TestWorkBroker {
        /// A broker that applies H2's refusals (symlink, non-regular, nlink,
        /// size) and hands out H2's sealed read-only memfd snapshot.
        pub fn new(root: PathBuf, run: RunId, employee: EmployeeId) -> Self {
            Self { root, live: Mutex::new(Some((run, employee))), fail: Mutex::new(None),
                misreport: Mutex::new(None), calls: Mutex::new(Vec::new()), checked: true }
        }
        /// A faulty broker that returns whatever it opened (the live inode, no
        /// snapshot), so tests can prove the boundary's own checks on the
        /// received descriptor.
        pub fn unchecked(root: PathBuf, run: RunId, employee: EmployeeId) -> Self {
            Self { checked: false, ..Self::new(root, run, employee) }
        }
        /// The run exits / its storage lease is detached.
        pub fn end_run(&self) { *self.live.lock().unwrap() = None; }
        pub fn fail_with(&self, error: Option<WorkBrokerError>) { *self.fail.lock().unwrap() = error; }
        pub fn misreport(&self, edit: impl Fn(&mut WorkFileMetadata) + Send + Sync + 'static) {
            *self.misreport.lock().unwrap() = Some(Box::new(edit));
        }
        pub fn calls(&self) -> Vec<(RunId, EmployeeId, String)> { self.calls.lock().unwrap().clone() }
    }
    impl WorkBroker for TestWorkBroker {
        fn open_work_file(&self, request: &WorkFileRequest<'_>) -> Result<BrokeredWorkFile, WorkBrokerError> {
            self.calls.lock().unwrap().push((request.run(), request.employee(), request.path().as_str().to_owned()));
            if let Some(error) = *self.fail.lock().unwrap() { return Err(error); }
            if *self.live.lock().unwrap() != Some((request.run(), request.employee())) {
                return Err(WorkBrokerError::NotLive);
            }
            let path = self.root.join(request.path().as_str());
            if self.checked {
                let link = std::fs::symlink_metadata(&path).map_err(|_| WorkBrokerError::NotFound)?;
                if !link.file_type().is_file() || link.nlink() != 1 { return Err(WorkBrokerError::Refused); }
                if link.len() > request.max_bytes() { return Err(WorkBrokerError::TooLarge); }
            }
            let mut file = std::fs::File::open(&path).map_err(|_| WorkBrokerError::NotFound)?;
            if self.checked { file = sealed_snapshot(&mut file).map_err(|_| WorkBrokerError::Unavailable)?; }
            let stat = file.metadata().map_err(|_| WorkBrokerError::Unavailable)?;
            let mut metadata = WorkFileMetadata { regular: stat.is_file(), size: stat.len(),
                device: stat.dev(), inode: stat.ino(), nlink: stat.nlink() };
            if let Some(edit) = self.misreport.lock().unwrap().as_ref() { edit(&mut metadata); }
            Ok(BrokeredWorkFile { file, metadata })
        }
    }
}

#[cfg(target_os = "linux")]
fn open_resource(root: &File, path: &Path, directory: bool) -> Result<File, VerificationError> {
    use rustix::fs::{fstatvfs, openat2, statx, AtFlags, FileType, Mode, OFlags, ResolveFlags,
        StatVfsMountFlags, StatxFlags};
    // Verify nodev before opening anything employee-controlled. NO_XDEV keeps
    // the target on this mount, so swapped devices cannot acquire device access.
    if !fstatvfs(root).map_err(|_| VerificationError::Unavailable)?.f_flag.contains(StatVfsMountFlags::NODEV) {
        return Err(VerificationError::Unavailable);
    }
    // NONBLOCK prevents a swapped FIFO from hanging; NOCTTY prevents terminal
    // acquisition. Roots are employee mounts, never arbitrary host directories.
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY;
    let fd = openat2(root, path, if directory { flags | OFlags::DIRECTORY } else { flags },
        Mode::empty(), ResolveFlags::BENEATH
        | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS | ResolveFlags::NO_XDEV)
        .map_err(|_| VerificationError::Unavailable)?;
    let stat = statx(&fd, "", AtFlags::EMPTY_PATH, StatxFlags::TYPE | StatxFlags::NLINK)
        .map_err(|_| VerificationError::Unavailable)?;
    let required = (StatxFlags::TYPE | StatxFlags::NLINK).bits();
    let kind = FileType::from_raw_mode(stat.stx_mode.into());
    if stat.stx_mask & required != required
        || (directory && kind != FileType::Directory)
        || (!directory && (kind != FileType::RegularFile || stat.stx_nlink != 1)) {
        return Err(VerificationError::Invalid);
    }
    Ok(File::from(fd))
}

#[cfg(not(target_os = "linux"))]
fn open_resource(_root: &File, _path: &Path, _directory: bool) -> Result<File, VerificationError> {
    Err(VerificationError::Unavailable)
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
