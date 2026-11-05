use std::{collections::BTreeMap, fs::File, path::{Component, Path}};
use duduclaw_isolation::{capability::{CapabilityRecord, EmployeeBinding, ResourceGrant, RequestContext,
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
        let selected = ["path", "file_path", "input_path", "source_path"]
            .iter().filter_map(|key| self.binding.arguments.get(key))
            .map(|value| value.as_str().ok_or(VerificationError::Invalid))
            .collect::<Result<Vec<_>, _>>()?;
        if selected.is_empty() || selected.iter().any(|selected| *selected != relative) {
            return Err(VerificationError::BindingMismatch);
        }
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

/// A regular-file descriptor selected through the call's workspace dirfd. Its
/// grant, call binding and dev/inode travel together through worker admission.
pub struct AuthorizedFile {
    binding: CallBinding,
    grant: ResourceGrant,
    file: File,
    identity: (u64, u64),
    invocation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl AuthorizedFile {
    fn from_opened(binding: CallBinding, grant: ResourceGrant, file: File) -> Result<Self, VerificationError> {
        if !file.metadata().map_err(|_| VerificationError::Unavailable)?.is_file() {
            return Err(VerificationError::Invalid);
        }
        let identity = file_identity(&file)?;
        Ok(Self { binding, grant, file, identity, invocation: None })
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

/// Server-owned roots, scoped to an employee. Register from trusted mount/dirfd
/// delivery only; never open a path supplied by an MCP client as a root.
#[derive(Default)]
pub struct ResourceRoots {
    roots: BTreeMap<String, (EmployeeId, File)>,
    wiki_roots: BTreeMap<String, (EmployeeId, File)>,
}
impl ResourceRoots {
    /// File tools have exactly one explicit workspace selector. Resolve it here
    /// so the table adapter cannot manufacture an unrelated regular-file input.
    pub fn authorize_file(&self, context: &RequestContext, tool: &str, arguments: &serde_json::Value)
        -> Result<(crate::tool_policy::AuthorizedToolCall, AuthorizedFile), VerificationError> {
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
        let input = self.authorize_workspace(&call, handle)?.open_file(path)?;
        Ok((call, input))
    }
    /// Mint authority only for a workspace selected by this call's resolver.
    pub fn authorize_workspace(&self, call: &crate::tool_policy::AuthorizedToolCall, handle: &str)
        -> Result<AuthorizedWorkspace, VerificationError> {
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
        let grant = workspaces.into_iter().find(|grant|
            matches!(grant, ResourceGrant::Workspace { handle: selected } if selected.as_str() == handle))
            .ok_or(VerificationError::BindingMismatch)?;
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
            || self.roots.len() >= 128 || !directory.metadata().map_err(|_| VerificationError::Unavailable)?.is_dir() {
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

    fn authorized_root(&self, context: &RequestContext, handle: &str) -> Result<&File, VerificationError> {
        let (owner, root) = self.roots.get(handle).ok_or(VerificationError::Invalid)?;
        if owner != &context.record().employee || !context.record().io_resources()
            .any(|r| matches!(r, ResourceGrant::Workspace { handle: h } if h.as_str() == handle)) {
            return Err(VerificationError::Invalid);
        }
        Ok(root)
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
