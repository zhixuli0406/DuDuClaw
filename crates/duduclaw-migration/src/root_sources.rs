//! Inventory only: legacy formats are parsed exclusively by the DynamicUser worker.
use std::{fs::File, path::{Path,PathBuf}, os::unix::ffi::OsStrExt};
use rustix::{fd::OwnedFd, fs::{self, OFlags,Mode,ResolveFlags,FileType}};
use serde::{Serialize,Deserialize};
use sha2::{Digest,Sha256};
use crate::{Error,ids::{RequestId,EmployeeId}};

const RESOLVE:ResolveFlags=ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS).union(ResolveFlags::NO_XDEV);
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub relative:PathBuf,
    pub kind:String,
    pub agent:Option<String>,
}
#[derive(Default,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory { pub sources:Vec<Source>,pub skipped:u64,#[serde(default)] pub departments:Vec<String> }
fn agent_id(value:&str)->bool {
    !value.is_empty() && value.as_bytes().first()!=Some(&b'_') && value.len()<=64 && value.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'))
}
/// Exact components, never substring matching. Non-authority work products remain
/// in quarantine until confirmed UID/quota allocation and the root blob copier.
pub fn classify(path:&Path)->Option<Source> {
    let text=path.to_str()?;
    let parts:Vec<_>=text.split('/').collect();
    if parts.iter().any(|p|p.is_empty()||matches!(*p,"."|"..")) {return None;}
    let agent=if parts.first()==Some(&"agents") {
        let id=*parts.get(1)?;if !agent_id(id) {return None;}Some(id.to_string())
    } else {None};
    let tail=if agent.is_some(){parts.get(2..)?}else{parts.as_slice()};
    let kind=match tail {
        ["config.toml"|"org.toml"|"users.db"|"tasks.db"|"approvals.db"|"mcp_keys.db"|"agent.toml"|".mcp.json"|"SOUL.md"|"IDENTITY.md"|"INSTRUCTIONS.md"|"CONTRACT.toml"
            |"roles.json"|"departments.json"|"channel_bindings.json"|"connectors.json"|"data_sources.json"|"approvals.json"
            |"schedules.json"|"cron.json"|"reminders.json"|"workflows.json"|"autopilot.json"|"responsibilities.json"|"hooks.json"|"agent_bindings.json"|"access_control.json"|"mcp-oauth-configs.json"|"mcp-oauth-tokens.json"
            |"workflow.db"|"autopilot.db"|"cron_tasks.db"|"cron_tasks.jsonl"|"reminders.jsonl"|"KILLSWITCH.toml"|"USER.md"]=>"authority",
        ["wiki",".scope.toml"]=>"authority",
        ["remote_mcp","servers.json"]|["person_credentials","store.json"]|["secrets","website_credentials.enc"]=>"authority",
        ["shared","wiki",".scope.toml"]|["shared","wiki","departments",_,".scope.toml"]=>"authority",
        ["redaction","profiles",_]|["shared","skills",..]|["soul_hashes",_]=>"authority",
        [".soul_hash"]|[".soul_hashes",_]|[".soul_history",_] if agent.is_some()=>"authority",
        [".claude","settings.json"] if agent.is_some()=>"authority",
        ["SKILLS"|"skills",..]=>"authority",
        ["hooks"|"policies"|"connectors"|"data-sources"|"workflows"|"responsibilities"|"autopilot",..]=>"authority",
        ["sessions.db"]=>"sessions_sqlite",
        ["memory.db"]=>"memory_sqlite",
        ["MEMORY.md"] if agent.is_some()=>"memory_text",
        ["wiki",..] if agent.is_some() && path.extension().is_some_and(|e|e=="md")=>"wiki_text",
        ["tasks",..] if agent.is_some()=>"task_archive",
        ["tool_calls.jsonl"]=>"audit_archive",
        ["attachments",..] if agent.is_some()=>"attachment",
        _ if agent.is_some() && !tail.iter().any(|p| matches!(*p,".claude"|".grok"|".codex"|"state"|"tmp") || p.starts_with(".soul_"))
            && !tail.first().is_some_and(|p|p.starts_with('.'))
            && path.extension().and_then(|e|e.to_str()).is_some_and(|e|matches!(e,"docx"|"xlsx"|"pdf"|"csv"|"md"|"txt"|"json"|"png"|"jpg"|"jpeg"|"svg"))=>"work_product",
        _=>return None,
    };
    Some(Source{relative:path.into(),kind:kind.into(),agent})
}
pub fn open_root(path:&Path)->Result<OwnedFd,Error> {
    fs::openat2(fs::CWD,path,OFlags::RDONLY|OFlags::DIRECTORY|OFlags::NOFOLLOW|OFlags::CLOEXEC,
        Mode::empty(),ResolveFlags::NO_SYMLINKS|ResolveFlags::NO_MAGICLINKS).map_err(|_|Error::Storage)
}
pub fn open_source(root:&OwnedFd,path:&Path)->Result<File,Error> {
    let fd=fs::openat2(root,path,OFlags::RDONLY|OFlags::NOFOLLOW|OFlags::CLOEXEC|OFlags::NONBLOCK,Mode::empty(),RESOLVE).map_err(|_|Error::Storage)?;
    let stat=fs::fstat(&fd).map_err(|_|Error::Storage)?;
    if FileType::from_raw_mode(stat.st_mode)!=FileType::RegularFile || stat.st_nlink!=1 || stat.st_size<0 || stat.st_size as u64>256*1024*1024 {return Err(Error::Invalid);}
    Ok(File::from(fd))
}
pub fn inventory(root:&OwnedFd)->Result<Inventory,Error> {
    fn visit(parent:&OwnedFd,relative:&Path,depth:u32,count:&mut u32,result:&mut Inventory)->Result<(),Error> {
        if depth>32 {return Err(Error::Limit);}
        let mut dir=fs::Dir::read_from(parent).map_err(|_|Error::Storage)?;
        while let Some(entry)=dir.read() {
            let entry=entry.map_err(|_|Error::Storage)?;let name=entry.file_name().to_bytes();
            if name==b"."||name==b".." {continue;}
            *count+=1;if *count>100_000 || depth>32 {return Err(Error::Limit);}
            let name=std::ffi::OsStr::from_bytes(name);let path=relative.join(name);
            let handle=fs::openat2(parent,name,OFlags::PATH|OFlags::NOFOLLOW|OFlags::CLOEXEC,Mode::empty(),RESOLVE).map_err(|_|Error::Storage)?;
            let stat=fs::fstat(&handle).map_err(|_|Error::Storage)?;
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory=>{
                    let parts:Vec<_>=path.components().collect();
                    if parts.len()==4 && parts[0].as_os_str()=="shared" && matches!(parts[1].as_os_str().to_str(),Some("wiki"|"skills"))
                        && parts[2].as_os_str()=="departments" {
                        if let Some(name)=name.to_str().filter(|n|!n.is_empty()&&n.len()<=64&&n.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'))) {
                            result.departments.push(name.to_string());
                            if result.departments.len()>2048 {return Err(Error::Limit);}
                        }else{result.skipped+=1;}
                    }
                    let cache_level=parts.len()==1 || (parts.len()==3 && parts[0].as_os_str()=="agents");
                    if cache_level && [".claude",".grok",".codex","state","tmp"].iter().any(|s|name==*s) {
                        // Hooks are authoritative proposals. Only this exact
                        // settings file enters the parser; CLI credentials,
                        // transcripts and every other cache entry stay behind.
                        if parts.len()==3 && name==".claude" {
                            let settings=path.join("settings.json");
                            let relative_settings=Path::new(name).join("settings.json");
                            match fs::openat2(parent,&relative_settings,OFlags::PATH|OFlags::NOFOLLOW|OFlags::CLOEXEC,Mode::empty(),RESOLVE) {
                                Ok(fd)=>{let metadata=fs::fstat(&fd).map_err(|_|Error::Storage)?;
                                    if FileType::from_raw_mode(metadata.st_mode)==FileType::RegularFile && metadata.st_nlink==1 {
                                        if let Some(source)=classify(&settings) {result.sources.push(source);}
                                    }},
                                Err(rustix::io::Errno::NOENT)=>{},
                                Err(_)=>return Err(Error::Storage),
                            }
                        }
                        result.skipped+=1;continue;
                    }
                    let child=fs::openat2(parent,name,OFlags::RDONLY|OFlags::DIRECTORY|OFlags::NOFOLLOW|OFlags::CLOEXEC,Mode::empty(),RESOLVE).map_err(|_|Error::Storage)?;
                    let opened=fs::fstat(&child).map_err(|_|Error::Storage)?;
                    if opened.st_dev!=stat.st_dev || opened.st_ino!=stat.st_ino || opened.st_mode!=stat.st_mode {return Err(Error::Conflict);}
                    visit(&child,&path,depth+1,count,result)?;
                }
                FileType::RegularFile if stat.st_nlink==1=>{
                    if let Some(source)=classify(&path) {
                        // Source files with global memory/session namespaces are
                        // expanded for each discovered employee below.
                        result.sources.push(source);
                    } else {result.skipped+=1;}
                }
                _=>result.skipped+=1,
            }
        }
        Ok(())
    }
    let mut result=Inventory::default();visit(root,Path::new(""),0,&mut 0,&mut result)?;
    result.sources.sort_by(|a,b|a.relative.cmp(&b.relative));
    result.departments.sort();result.departments.dedup();
    let agents:std::collections::BTreeSet<_>=result.sources.iter().filter_map(|s|s.agent.clone()).collect();
    if agents.len()>crate::contract::DEFAULT_EMPLOYEE_PROPOSAL_LIMIT as usize {return Err(Error::Limit);}
    let sources=std::mem::take(&mut result.sources);
    for source in sources {
        if source.agent.is_none() && matches!(source.kind.as_str(),"sessions_sqlite"|"memory_sqlite") {
            if source.kind=="sessions_sqlite" {let mut settings=source.clone();settings.kind="authority".into();result.sources.push(settings);}
            for agent in &agents {let mut split=source.clone();split.agent=Some(agent.clone());result.sources.push(split);}
            if agents.is_empty(){result.skipped+=1;}
        } else {result.sources.push(source);}
        if result.sources.len()>100_000 {return Err(Error::Limit);}
    }
    Ok(result)
}
pub fn source_id(migration:RequestId,source:&Source)->String {
    let encoded=serde_json::to_vec(&(migration,&source.relative,&source.agent,&source.kind)).expect("source serializes");
    Sha256::digest(encoded).iter().map(|b|format!("{b:02x}")).collect()
}
pub fn employee_id(migration:RequestId,agent:&str)->EmployeeId {
    let mut hash=Sha256::new();hash.update(b"migration-employee-v1\0");hash.update(migration.0);hash.update(agent.as_bytes());
    let hash=hash.finalize();let mut id=[0;16];id.copy_from_slice(&hash[..16]);EmployeeId(id)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_source_routes_authority_only_to_proposals() {
        for path in ["users.db","config.toml","org.toml","tasks.db","agents/sales/agent.toml","agents/sales/.mcp.json","agents/sales/SOUL.md","agents/sales/wiki/.scope.toml","agents/sales/SKILLS/a/SKILL.md","roles.json","departments.json","channel_bindings.json","approvals.json","connectors.json","data_sources.json","cron.json","agents/sales/hooks.json","agents/sales/.claude/settings.json"] {
            assert_eq!(classify(Path::new(path)).unwrap().kind,"authority");
        }
        for path in ["agents/sales/.claude/credentials.json","agents/sales/state/working_state.json","agents/sales/.mcp.json.bak","agents/../agent.toml","agents/_internal/agent.toml"] {assert!(classify(Path::new(path)).is_none());}
        assert_eq!(classify(Path::new("agents/sales/wiki/hello.md")).unwrap().kind,"wiki_text");
    }
    #[test]
    fn migration_source_inventory_refuses_links_and_limits_employees() {
        let tmp=tempfile::tempdir().unwrap();std::fs::write(tmp.path().join("config.toml"),"test").unwrap();
        std::os::unix::fs::symlink("config.toml",tmp.path().join("org.toml")).unwrap();
        let root=open_root(tmp.path()).unwrap();let found=inventory(&root).unwrap();
        assert_eq!(found.sources.len(),1);assert_eq!(found.skipped,1);
        assert!(open_source(&root,Path::new("org.toml")).is_err());
    }
}

#[cfg(test)]
mod traversal_tests {
    use super::*;
    #[test]
    fn migration_source_preserves_nested_wiki_state_and_refuses_root_symlinks() {
        let tmp=tempfile::tempdir().unwrap();
        let page=tmp.path().join("agents/sales/wiki/state/notes.md");
        std::fs::create_dir_all(page.parent().unwrap()).unwrap();std::fs::write(&page,"content").unwrap();
        let cache=tmp.path().join("agents/sales/state/private.json");
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();std::fs::write(&cache,"secret").unwrap();
        let root=open_root(tmp.path()).unwrap();let found=inventory(&root).unwrap();
        assert_eq!(found.sources.len(),1);assert_eq!(found.sources[0].relative,Path::new("agents/sales/wiki/state/notes.md"));
        let alias=tmp.path().join("alias");std::os::unix::fs::symlink(tmp.path().join("agents"),&alias).unwrap();
        assert!(open_root(&alias.join("sales")).is_err());
    }
    #[test]
    fn migration_source_inventory_limits_employee_count() {
        let tmp=tempfile::tempdir().unwrap();
        for id in 0..=200 {let path=tmp.path().join(format!("agents/employee-{id}/agent.toml"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();std::fs::write(path,"name='test'").unwrap();}
        assert!(matches!(inventory(&open_root(tmp.path()).unwrap()),Err(Error::Limit)));
    }
}

#[cfg(test)]
mod hooks_tests {
    use super::*;
    #[test]
    fn migration_source_reads_only_hook_settings_from_cli_cache() {
        let tmp=tempfile::tempdir().unwrap();let cache=tmp.path().join("agents/sales/.claude");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("settings.json"),"{}").unwrap();
        std::fs::write(cache.join("credentials.json"),"secret").unwrap();
        let found=inventory(&open_root(tmp.path()).unwrap()).unwrap();
        assert_eq!(found.sources.len(),1);
        assert_eq!(found.sources[0].relative,Path::new("agents/sales/.claude/settings.json"));
        assert_eq!(found.sources[0].kind,"authority");
    }
}

#[cfg(test)]
mod actual_stores_tests {
    use super::*;
    #[test]
    fn migration_source_recognizes_actual_authority_stores_and_baselines() {
        for path in ["approvals.db","mcp_keys.db","agent_bindings.json","access_control.json","remote_mcp/servers.json","person_credentials/store.json",
            "mcp-oauth-configs.json","mcp-oauth-tokens.json","secrets/website_credentials.enc","KILLSWITCH.toml","redaction/profiles/custom.toml",
            "workflow.db","autopilot.db","cron_tasks.db","cron_tasks.jsonl","reminders.jsonl","shared/wiki/.scope.toml","shared/skills/departments/sales/test/SKILL.md",
            "agents/sales/.soul_hash","agents/sales/.soul_hashes/sales.hash","agents/sales/.soul_history/SOUL_20260101.md","soul_hashes/sales.hash",
            "agents/sales/USER.md","agents/sales/SKILLS/a/scripts/run.sh"] {
            assert_eq!(classify(Path::new(path)).unwrap().kind,"authority","{path}");
        }
    }
    #[test]
    fn migration_source_session_content_and_channel_settings_have_distinct_ids() {
        let tmp=tempfile::tempdir().unwrap();let agent=tmp.path().join("agents/sales");std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"),"").unwrap();std::fs::write(tmp.path().join("sessions.db"),"").unwrap();
        let found=inventory(&open_root(tmp.path()).unwrap()).unwrap();
        let session_sources:Vec<_>=found.sources.iter().filter(|s|s.relative==Path::new("sessions.db")).collect();
        assert_eq!(session_sources.len(),2);
        assert_ne!(source_id(RequestId([1;16]),session_sources[0]),source_id(RequestId([1;16]),session_sources[1]));
    }
}

#[cfg(test)]
mod department_tests {
    use super::*;
    #[test]
    fn migration_source_inventory_keeps_empty_department_proposals() {
        let tmp=tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("shared/wiki/departments/sales")).unwrap();
        std::fs::create_dir_all(tmp.path().join("shared/skills/departments/sales")).unwrap();
        let found=inventory(&open_root(tmp.path()).unwrap()).unwrap();
        assert_eq!(found.departments,vec!["sales"]);assert!(found.sources.is_empty());
    }
}
