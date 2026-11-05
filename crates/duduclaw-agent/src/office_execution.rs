//! Execute only manifest-verified embedded code, with private input/output snapshots.
//! Skill layers supply prompts, never executable bytes. Linux/macOS only until equivalent
//! no-follow creation and private-directory guarantees exist on other platforms.
pub mod manifest;
pub mod process;
mod isolation;
mod schema;
mod cache;

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use crate::skill_loader::safe_dir::SafeDir;

const MAX_INPUT: u64 = 64 * 1024 * 1024;
const MAX_OUTPUT: u64 = 128 * 1024 * 1024;

pub struct PreparedOffice {
    home: PathBuf,
    workspace: tempfile::TempDir,
    #[cfg(unix)]
    workspace_dir: SafeDir,
    script: PathBuf,
    input: PathBuf,
    args: Vec<String>,
    agent_root: PathBuf,
    cache: PathBuf,
    base_cache: PathBuf,
    run_cache: PathBuf,
    output: Option<(PathBuf, PathBuf)>,
    skill: String,
    stem: String,
}

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn normal_relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.components().any(|c| !matches!(c, Component::Normal(_)))
        || path.to_string_lossy().contains('\\') {
        return Err(refused("Path escapes the agent sandbox or contains non-normal components"));
    }
    Ok(())
}

fn parent_at(root: &Path, relative: &Path) -> io::Result<(SafeDir, String)> {
    normal_relative(relative)?;
    let mut dir = SafeDir::open(root)?;
    let name = relative.file_name().and_then(|s| s.to_str())
        .ok_or_else(|| refused("Invalid document filename"))?.to_owned();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            dir = dir.child(component.as_os_str().to_str()
                .ok_or_else(|| refused("Invalid document directory"))?)?;
        }
    }
    Ok((dir, name))
}

fn under(path: &Path, root: &Path) -> io::Result<PathBuf> {
    let relative = path.strip_prefix(root)
        .map_err(|_| refused("Path escapes the agent sandbox"))?;
    normal_relative(relative)?;
    Ok(relative.to_path_buf())
}

#[cfg(unix)]
fn private(dir: &SafeDir) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = dir.metadata()?;
    // The directory belongs to the gateway/MCP uid, not an employee-controlled layer.
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
        return Err(refused("Office execution directory must be owned by the gateway uid with mode 0700"));
    }
    Ok(())
}

impl PreparedOffice {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn new(_: &Path, _: &Path, _: &str, _: &str, _: &[String]) -> io::Result<Self> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Secure office execution requires Linux or macOS"))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn new(home: &Path, agent: &Path, skill: &str, stem: &str, args: &[String]) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let parsed = schema::parse(skill, stem, args)?;
        let bytes = manifest::embedded_script(skill, stem)?;
        // The configured home is the trust anchor (macOS may name it through /var).
        // Every component BELOW it, including the agent root, is opened no-follow.
        let relative_agent = under(agent, home)?;
        let configured_home = home;
        let home = home.canonicalize()?;
        let agent_root = home.join(relative_agent);
        SafeDir::open(&agent_root)?;
        let input_arg = Path::new(&parsed.input);
        let input_path = if input_arg.is_absolute() {
            // Translate only the configured home alias, never canonicalize untrusted components first.
            if let Ok(rel) = input_arg.strip_prefix(configured_home) { home.join(rel) }
            else { input_arg.to_path_buf() }
        } else { agent_root.join(input_arg) };
        let attachments = home.join("attachments");
        let (input_root, input_relative) = if let Ok(relative) = under(&input_path, &agent_root) {
            (&agent_root, relative)
        } else { (&attachments, under(&input_path, &attachments)?) };
        let (input_parent, input_name) = parent_at(input_root, &input_relative)?;
        let source = input_parent.read_file(&input_name)?;
        let canonical = input_path.canonicalize()?;
        if canonical != input_path || !canonical.starts_with(input_root) {
            return Err(refused("Input contains a symlink component"));
        }
        if source.metadata()?.len() > MAX_INPUT { return Err(refused("Office input exceeds 64 MiB")); }

        let output = if stem == "extract" { None } else {
            let target = if parsed.conversion {
                let parent = match &parsed.output {
                    Some(out) => {
                        let p = Path::new(out);
                        if p.is_absolute() { home.join(p.strip_prefix(configured_home).unwrap_or(p)) } else { agent_root.join(p) }
                    }
                    None => input_path.parent().unwrap().to_path_buf(),
                };
                parent.join(Path::new(&input_name).with_extension("pdf"))
            } else {
                let p = Path::new(parsed.output.as_ref().unwrap());
                if p.is_absolute() { home.join(p.strip_prefix(configured_home).unwrap_or(p)) } else { agent_root.join(p) }
            };
            let relative = under(&target, &agent_root)?;
            let (_, name) = parent_at(&agent_root, &relative)?;
            if duduclaw_core::AGENT_STRUCTURE_FILES.iter().any(|p| p.eq_ignore_ascii_case(&name)) {
                return Err(refused("Protected agent structure file"));
            }
            let extension = if parsed.conversion { "pdf" } else { skill };
            if target.extension().and_then(|v| v.to_str()) != Some(extension) {
                return Err(refused("Output extension must match the selected office format"));
            }
            // Publishing uses exclusive no-replace rename; this check only avoids doing unnecessary work.
            match std::fs::symlink_metadata(&target) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
                Ok(_) => return Err(refused("Output already exists; choose a new filename")),
            }
            Some((relative, extension))
        };

        let execution_root = SafeDir::open(&home)?.mkdir_child(".office-execution", 0o700)?;
        private(&execution_root)?;
        private(&execution_root.mkdir_child("uv-cache", 0o700)?)?;
        let cache = home.join(".office-execution/uv-cache");
        // Never promote the legacy parser-writable cache into a trusted base.
        private(&execution_root.mkdir_child("uv-base-v1", 0o700)?)?;
        let base_cache = home.join(".office-execution/uv-base-v1");
        let workspace = tempfile::Builder::new()
            .prefix(&format!("{}-", manifest::VERSION))
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(home.join(".office-execution"))?;
        let workspace_dir = SafeDir::open(workspace.path())?;
        private(&workspace_dir)?;
        private(&workspace_dir.mkdir_child("uv-cache", 0o700)?)?;
        let run_cache = workspace.path().join("uv-cache");
        let script = workspace.path().join("script.py");
        let mut executable = workspace_dir.create_new("script.py", 0o500)?;
        executable.write_all(bytes)?;
        executable.sync_all()?;
        executable.set_permissions(std::fs::Permissions::from_mode(0o500))?;
        drop(executable);
        let mut check = Vec::new();
        workspace_dir.read_file("script.py")?.read_to_end(&mut check)?;
        manifest::verify_script(skill, &format!("scripts/{stem}.py"), &check)?;

        let ext = input_path.extension().and_then(|v| v.to_str()).unwrap_or("data");
        if !ext.chars().all(|c| c.is_ascii_alphanumeric()) { return Err(refused("Invalid input extension")); }
        let staged_input_name = format!("input.{ext}");
        let input = workspace.path().join(&staged_input_name);
        let mut destination = workspace_dir.create_new(&staged_input_name, 0o400)?;
        let copied = io::copy(&mut source.take(MAX_INPUT + 1), &mut destination)?;
        if copied > MAX_INPUT { return Err(refused("Office input exceeds 64 MiB")); }
        destination.sync_all()?;
        let mut rewritten = vec![input.to_string_lossy().into_owned()];
        let output = output.map(|(relative, ext)| {
            let staged = workspace.path().join(if parsed.conversion { "input.pdf".to_owned() } else { format!("output.{ext}") });
            if parsed.conversion {
                rewritten.extend(["--outdir".into(), workspace.path().to_string_lossy().into_owned()]);
            } else {
                rewritten.extend(["--out".into(), staged.to_string_lossy().into_owned()]);
            }
            (staged, relative)
        });
        if let Some(format) = parsed.format { rewritten.extend(["--format".into(), format]); }
        Ok(Self { home, workspace, workspace_dir, script, input, args: rewritten, agent_root, cache, base_cache, run_cache, output,
            skill: skill.to_owned(), stem: stem.to_owned() })
    }

    pub fn script(&self) -> &Path { &self.script }
    pub fn input(&self) -> &Path { &self.input }
    pub fn args(&self) -> &[String] { &self.args }
    pub fn workspace(&self) -> &Path { self.workspace.path() }
    pub fn cache(&self) -> &Path { &self.cache }
    pub fn base_cache(&self) -> &Path { &self.base_cache }
    pub fn run_cache(&self) -> &Path { &self.run_cache }

    fn cache_marker(&self) -> io::Result<String> {
        let bytes = manifest::embedded_script(&self.skill, &self.stem)?;
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
        let digest: String = digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(format!("office-warm-{digest}"))
    }

    /// A prior successful run permits offline reuse only for this script and cutoff.
    /// Cache eviction may make a marked cache unusable; never retry a script online.
    #[cfg(unix)]
    pub fn cache_is_warm(&self) -> io::Result<bool> {
        self.cache_is_warm_at(self.cache())
    }

    #[cfg(unix)]
    fn cache_is_warm_at(&self, path: &Path) -> io::Result<bool> {
        let cache = SafeDir::open(path)?;
        private(&cache)?;
        let file = match cache.read_file(&self.cache_marker()?) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut value = String::new();
        file.take(128).read_to_string(&mut value)?;
        if value != manifest::EXCLUDE_NEWER { return Err(refused("Invalid office cache marker")); }
        Ok(true)
    }

    #[cfg(unix)]
    pub fn mark_cache_warm(&self) -> io::Result<()> {
        self.mark_cache_warm_at(self.cache())
    }

    #[cfg(unix)]
    fn mark_cache_warm_at(&self, path: &Path) -> io::Result<()> {
        if self.cache_is_warm_at(path)? { return Ok(()); }
        let cache = SafeDir::open(path)?;
        private(&cache)?;
        match cache.create_new(&self.cache_marker()?, 0o600) {
            Ok(mut file) => { file.write_all(manifest::EXCLUDE_NEWER.as_bytes())?; file.sync_all() }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if self.cache_is_warm_at(path)? { Ok(()) } else { Err(error) }
            }
            Err(error) => Err(error),
        }
    }

    #[cfg(not(unix))]
    pub fn cache_is_warm(&self) -> io::Result<bool> { Ok(false) }

    #[cfg(not(unix))]
    pub fn mark_cache_warm(&self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Secure office execution requires Unix"))
    }

    /// Rewalk all private paths immediately before spawning. Keep the verified
    /// descriptor open through spawn; pathname execution still has a same-uid race.
    #[cfg(unix)]
    pub fn verify_before_spawn(&self) -> io::Result<std::fs::File> {
        use std::os::unix::fs::MetadataExt;
        private(&SafeDir::open(self.cache.parent().ok_or_else(|| refused("Missing execution root"))?)?)?;
        private(&SafeDir::open(self.cache())?)?;
        private(&SafeDir::open(self.base_cache())?)?;
        private(&SafeDir::open(self.run_cache())?)?;
        let dir = SafeDir::open(self.workspace())?;
        private(&dir)?;
        let mut file = dir.read_file("script.py")?;
        let metadata = file.metadata()?;
        let expected = manifest::embedded_script(&self.skill, &self.stem)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o500
            || metadata.len() != expected.len() as u64 {
            return Err(refused("Office script ownership, permissions or length changed"));
        }
        let mut bytes = Vec::new();
        (&mut file).take(expected.len() as u64 + 1).read_to_end(&mut bytes)?;
        manifest::verify_script(&self.skill, &format!("scripts/{}.py", self.stem), &bytes)?;
        Ok(file)
    }

    #[cfg(not(unix))]
    pub fn verify_before_spawn(&self) -> io::Result<std::fs::File> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Secure office execution requires Unix"))
    }

    /// Read dependency pins only from verified compiled bytes, never from disk.
    pub fn dependency_pins(&self) -> io::Result<Vec<String>> {
        let bytes = manifest::embedded_script(&self.skill, &self.stem)?;
        let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
        let metadata = text.lines().skip(1).take_while(|line| *line != "# ///")
            .map(|line| line.strip_prefix("# ").unwrap_or("")).collect::<Vec<_>>().join("\n");
        let metadata: toml::Value = toml::from_str(&metadata).map_err(io::Error::other)?;
        let deps = metadata.get("dependencies").and_then(toml::Value::as_array)
            .ok_or_else(|| refused("Missing embedded dependency metadata"))?;
        deps.iter().map(|dep| {
            let pin = dep.as_str().ok_or_else(|| refused("Invalid embedded dependency"))?;
            let (name, version) = pin.split_once("==").ok_or_else(|| refused("Unpinned embedded dependency"))?;
            if name.is_empty() || version.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
                || !version.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
                return Err(refused("Invalid embedded dependency pin"));
            }
            Ok(pin.to_owned())
        }).collect()
    }

    /// Only Rust publishes output. Python never opens an employee-selected output path.
    #[cfg(unix)]
    pub fn publish(&self) -> io::Result<Option<PathBuf>> {
        let Some((staged, relative)) = &self.output else { return Ok(None); };
        let source_dir = SafeDir::open(self.workspace())?;
        let source = source_dir.read_file(staged.file_name().unwrap().to_str().unwrap())?;
        if source.metadata()?.len() > MAX_OUTPUT { return Err(refused("Office output exceeds 128 MiB")); }
        // Complete the copy in the private directory first. A copy/sync failure
        // must not leave a partial user-visible file that blocks retries.
        let export_name = format!("publish-{}", uuid::Uuid::new_v4());
        let mut destination = source_dir.create_new(&export_name, 0o600)?;
        let copied = io::copy(&mut source.take(MAX_OUTPUT + 1), &mut destination)?;
        if copied > MAX_OUTPUT { return Err(refused("Office output exceeds 128 MiB")); }
        destination.sync_all()?;
        drop(destination);
        // Rewalk after the child exits; exclusive fd-relative rename cannot
        // follow a late symlink or replace an existing output. EXDEV fails closed.
        let (parent, name) = parent_at(&self.agent_root, relative)?;
        parent.publish_new(&source_dir, &export_name, &name)?;
        Ok(Some(self.agent_root.join(relative)))
    }

    #[cfg(not(unix))]
    pub fn publish(&self) -> io::Result<Option<PathBuf>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "Secure office publication is currently Unix-only"))
    }
}

#[cfg(unix)]
impl Drop for PreparedOffice {
    fn drop(&mut self) {
        // A compromised parser can chmod its own directories to 000. Repair
        // through the retained root fd before TempDir's otherwise silent cleanup.
        let result = self.workspace_dir.restore_tree_permissions()
            .and_then(|_| std::fs::remove_dir_all(self.workspace()));
        if let Err(error) = result {
            tracing::warn!(event = "office_workspace_cleanup_failed", path = %self.workspace().display(),
                %error, "Office disposable workspace cleanup failed");
        }
    }
}

#[cfg(test)]
mod tests;
