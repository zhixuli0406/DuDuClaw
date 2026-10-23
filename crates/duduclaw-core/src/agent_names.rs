//! Names an imported employee must not share with anyone already on the
//! team. Channel routing calls employees by `@<display name>` and `@<id>`
//! (and by their configured trigger), so a second employee answering to
//! the same word would make the call ambiguous. Every id, display name and
//! trigger in `<home>/agents/*/agent.toml` is collected once per import;
//! comparison is case-insensitive and ignores a leading `@`.
//!
//! Shared by every installer (imported employees, expert packs, AI-generated
//! teams) and by the dashboard's pre-install check, so they agree on what
//! counts as a collision.

use std::collections::BTreeSet;
use std::path::Path;

pub struct NameBook {
    taken: BTreeSet<String>,
}

/// The comparison key: trimmed, leading `@` dropped, lowercased.
pub fn norm(s: &str) -> String {
    s.trim().trim_start_matches('@').trim().to_lowercase()
}

impl NameBook {
    /// Every name the current employees answer to.
    pub fn load(home: &Path) -> Self {
        Self::load_excluding(home, &BTreeSet::new())
    }

    /// [`Self::load`] without the employees whose directory names are in
    /// `skip` — used when a re-import is about to replace exactly those
    /// employees, so their names are free for the new version.
    pub fn load_excluding(home: &Path, skip: &BTreeSet<String>) -> Self {
        let mut taken = BTreeSet::new();
        let Ok(rd) = std::fs::read_dir(home.join("agents")) else {
            return NameBook { taken };
        };
        for e in rd.flatten() {
            let dir_name = e.file_name().to_string_lossy().into_owned();
            if dir_name.starts_with('.') || dir_name.starts_with('_') || skip.contains(&dir_name) {
                continue;
            }
            taken.insert(norm(&dir_name));
            let Some(table) = std::fs::read_to_string(e.path().join("agent.toml"))
                .ok()
                .and_then(|t| t.parse::<toml::Table>().ok())
            else {
                continue;
            };
            let agent = table.get("agent").and_then(|v| v.as_table());
            for key in ["name", "display_name", "trigger"] {
                if let Some(v) = agent.and_then(|a| a.get(key)).and_then(|v| v.as_str())
                    && !norm(v).is_empty()
                {
                    taken.insert(norm(v));
                }
            }
        }
        NameBook { taken }
    }

    pub fn is_taken(&self, name: &str) -> bool {
        self.taken.contains(&norm(name))
    }

    pub fn reserve(&mut self, name: &str) {
        let n = norm(name);
        if !n.is_empty() {
            self.taken.insert(n);
        }
    }

    /// A display name nobody else answers to: `display` itself, else
    /// `display（id）`, else `display-id`. Reserves the result (and the id).
    /// Returns `(name, changed)`.
    pub fn unique_display(&mut self, display: &str, id: &str) -> (String, bool) {
        let candidates = [
            display.to_string(),
            format!("{display}（{id}）"),
            format!("{display}-{id}"),
        ];
        let chosen = candidates
            .iter()
            // Its own id is reserved already; a display name equal to it is fine.
            .find(|c| norm(c) == norm(id) || !self.is_taken(c))
            .cloned()
            .unwrap_or_else(|| format!("{display}-{}", uuid::Uuid::new_v4().simple()));
        let changed = chosen != display;
        self.reserve(&chosen);
        self.reserve(id);
        (chosen, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(home: &Path, id: &str, display: &str, trigger: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{id}\"\ndisplay_name = \"{display}\"\ntrigger = \"{trigger}\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn existing_ids_display_names_and_triggers_are_all_taken() {
        let home = tempfile::tempdir().unwrap();
        agent(home.path(), "sales", "業務小美", "@小美");
        let mut book = NameBook::load(home.path());
        assert!(book.is_taken("SALES") && book.is_taken("@業務小美") && book.is_taken("小美"));
        let (d, changed) = book.unique_display("業務小美", "doubao-a1");
        assert!(changed);
        assert_eq!(d, "業務小美（doubao-a1）");
        // The new name and id are reserved for the rest of the import.
        assert!(book.is_taken("doubao-a1"));
        let (d2, _) = book.unique_display("業務小美", "doubao-a1");
        assert_eq!(d2, "業務小美-doubao-a1");
        let (free, changed) = book.unique_display("報告顧問", "doubao-a2");
        assert_eq!((free.as_str(), changed), ("報告顧問", false));
    }

    #[test]
    fn excluded_employees_free_their_names() {
        let home = tempfile::tempdir().unwrap();
        agent(home.path(), "doubao-a1", "報告顧問", "@報告顧問");
        agent(home.path(), "sales", "業務小美", "@小美");
        let skip: BTreeSet<String> = ["doubao-a1".to_string()].into_iter().collect();
        let book = NameBook::load_excluding(home.path(), &skip);
        assert!(!book.is_taken("報告顧問") && !book.is_taken("doubao-a1"));
        assert!(book.is_taken("小美"));
    }
}
