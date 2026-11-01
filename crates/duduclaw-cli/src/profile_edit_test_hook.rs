//! Test-only interleaving at the boundary before a non-lifecycle profile edit.
use std::cell::RefCell;
use std::path::Path;

thread_local! {
    static BEFORE_EDIT: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

pub(crate) fn run() {
    let hook = BEFORE_EDIT.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook { hook(); }
}

/// Read the obsolete snapshot, finish a competing lifecycle write on another
/// thread, then allow the delayed field edit to proceed. No timing sleeps.
pub(crate) fn after_snapshot(home: &Path, agent: &str, old: &str, new: &str) {
    let (home, agent, old, new) = (home.to_path_buf(), agent.to_string(), old.to_string(), new.to_string());
    BEFORE_EDIT.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(move || {
            let snapshot = std::fs::read_to_string(home.join("agents").join(&agent).join("agent.toml")).unwrap();
            let snapshot: toml::Table = snapshot.parse().unwrap();
            assert_eq!(snapshot["agent"]["status"].as_str(), Some(old.as_str()));
            std::thread::spawn(move || {
                duduclaw_gateway::agent_files::external::edit_agent_toml(
                    &home, &agent, "test", "operator", |table| {
                        table.get_mut("agent").unwrap().as_table_mut().unwrap()
                            .insert("status".into(), toml::Value::String(new));
                        Ok(())
                    },
                ).unwrap();
            }).join().unwrap();
        }));
    });
}
