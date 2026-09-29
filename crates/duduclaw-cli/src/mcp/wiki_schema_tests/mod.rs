//! Karpathy-schema frontmatter guard + fallback-content rejection
//! (v1.8.26 shared wiki hygiene).
use super::*;
use std::fs;

/// Minimal TempDir — sibling modules can't share `tests::TempDir`.
struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let p =
            std::env::temp_dir().join(format!("duduclaw-wiki-schema-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_agent_toml(agents_dir: &std::path::Path, name: &str) {
    let dir = agents_dir.join(name);
    fs::create_dir_all(&dir).unwrap();
    // Minimal agent.toml — handle_shared_wiki_write doesn't parse it,
    // but other helpers invoked during the call might read [agent].role.
    fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = \"{name}\"\nrole = \"main\"\n"),
    )
    .unwrap();
}

fn write_scope_policy(home: &std::path::Path, body: &str) {
    let path = home.join("shared").join("wiki").join(".scope.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
}

fn clean_karpathy_page(title: &str) -> String {
    format!(
        "---\n\
             title: {title}\n\
             created: 2026-05-04T00:00:00Z\n\
             updated: 2026-05-04T00:00:00Z\n\
             tags: [test]\n\
             layer: context\n\
             trust: 0.5\n\
             ---\n\
             body content\n",
    )
}

fn write_agent_toml_dept(agents_dir: &std::path::Path, name: &str, department: &str) {
    let dir = agents_dir.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = \"{name}\"\nrole = \"main\"\ndepartment = \"{department}\"\n"),
    )
    .unwrap();
}

fn write_identity_record(home: &std::path::Path, filename: &str, frontmatter: &str) {
    let dir = home
        .join("shared")
        .join("wiki")
        .join("identity")
        .join("people");
    fs::create_dir_all(&dir).unwrap();
    let body = format!("---\n{frontmatter}---\n");
    fs::write(dir.join(filename), body).unwrap();
}

mod part1;
mod part2;
