use std::{collections::BTreeMap, fs::File, path::{Component, Path}};
use duduclaw_isolation::{capability::{CapabilityRecord, EmployeeBinding, ResourceGrant, RequestContext,
    VerificationError}, ids::EmployeeId};

/// Reject malformed resource policy before it can be signed.
pub(crate) fn validate_grants(record: &CapabilityRecord) -> Result<(), VerificationError> {
    for grant in record.resources.as_slice() {
        crate::tool_policy::validate_resource(grant)?;
        match grant {
            ResourceGrant::Workspace { handle } if handle.as_str().is_empty() => return Err(VerificationError::Invalid),
            ResourceGrant::Connector { binding: EmployeeBinding::Personal(owner), .. }
                if !record.personal_owner_matches(owner.as_str()) => return Err(VerificationError::Invalid),
            ResourceGrant::Connector { binding, .. } if binding != &record.employee_binding =>
                return Err(VerificationError::BindingMismatch),
            ResourceGrant::UrlOrigin { origin } => crate::tool_policy::validate_origin(origin.as_str())?,
            _ => {}
        }
    }
    Ok(())
}

pub fn require_resource(context: &RequestContext, resource: &ResourceGrant) -> Result<(), VerificationError> {
    if context.record().resources.as_slice().iter().any(|grant| grant == resource) { Ok(()) }
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

/// An owned authorization proof for a helper's workspace. Only ResourceRoots
/// can construct it; no pathname, environment variable or MCP value can mint it.
/// Root reopens the fixed handle under H and verifies this directory identity.
pub struct AuthorizedWorkspace {
    employee: EmployeeId,
    run: duduclaw_isolation::ids::RunId,
    policy: duduclaw_isolation::execution::WorkspacePolicy,
    _directory: File,
}
impl AuthorizedWorkspace {
    pub fn delivery(&self) -> Result<duduclaw_isolation::execution::WorkspaceDelivery, VerificationError> {
        Ok(duduclaw_isolation::execution::WorkspaceDelivery { employee: self.employee, run: self.run,
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
}
impl ResourceRoots {
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
        if owner != &context.record().employee || !context.record().resources.as_slice().iter()
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

    pub fn authorize_workspace(&self, context: &RequestContext,
        policy: &duduclaw_isolation::execution::WorkspacePolicy) -> Result<AuthorizedWorkspace, VerificationError> {
        let cwd = policy.relative_cwd.as_str();
        let directory = self.open_directory(context, policy.workspace.as_str(),
            if cwd.is_empty() || cwd == "." { None } else { Some(cwd) })?;
        Ok(AuthorizedWorkspace { employee: context.record().employee, run: context.record().run,
            policy: policy.clone(), _directory: directory })
    }

    fn authorized_root(&self, context: &RequestContext, handle: &str) -> Result<&File, VerificationError> {
        let (owner, root) = self.roots.get(handle).ok_or(VerificationError::Invalid)?;
        if owner != &context.record().employee || !context.record().resources.as_slice().iter()
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
