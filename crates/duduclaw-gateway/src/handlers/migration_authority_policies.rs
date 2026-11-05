//! Sections whose real product consumers read their strict policy per use.
//! This is an explicit catalogue, not a generic TOML publication escape hatch.
use super::*;
pub(super) fn section_supported(section:&str)->bool {matches!(section,"container"|"computer_use"|"provenance")}
pub(super) fn item_supported(item:&ConfirmationItem)->bool {
    // These are global product sections. A per-employee policy proposal must
    // never silently widen its scope into a global configuration write.
    if item.fields.iter().any(|f|f.name=="employee_id") {return false;}
    let key=match item.category {ConfirmationCategory::Policy=>"name",ConfirmationCategory::ConfigurationSection=>"section",_=>return false};
    text(item,key).is_ok_and(section_supported)
}
pub(super) fn validate(section:&str,table:&toml::Table)->Result<()> {
    let value=table.get(section).and_then(toml::Value::as_table).ok_or(Error::Invalid)?;
    match section {
        "container"=>{
            if value.keys().any(|k|k!="sandbox") || !value.get("sandbox").is_some_and(toml::Value::is_table) {return Err(Error::Invalid);}
            let sandbox=value.get("sandbox").and_then(toml::Value::as_table).ok_or(Error::Invalid)?;
            if ["when_unavailable","script_when_unavailable"].iter().any(|key|sandbox.get(*key).is_some_and(|v|!matches!(v.as_str(),Some("fail"|"run_unsandboxed")))) {return Err(Error::Invalid);}
            crate::task_sandbox::settings::parse(table).map_err(|_|Error::Invalid)?;
        },
        "computer_use"=>{
            crate::computer_use_image::parse(table).map_err(|_|Error::Invalid)?;
            crate::computer_workspaces::config::parse(table).map_err(|_|Error::Invalid)?;
        },
        "provenance"=>{
            if value.keys().any(|k|!matches!(k.as_str(),"policy"|"sensitive_tools")) {return Err(Error::Invalid);}
            if value.get("policy").is_some_and(|v|!matches!(v.as_str(),Some("off"|"warn"|"enforce"))) {return Err(Error::Invalid);}
            if let Some(tools)=value.get("sensitive_tools") {
                let tools=tools.as_array().ok_or(Error::Invalid)?;
                if tools.len()>256 || tools.iter().any(|v|v.as_str().is_none_or(|s|s.is_empty()||s.len()>128||!s.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'))) {return Err(Error::Invalid);}
            }
            let _=crate::channel_reply::parse_provenance_settings(table);
        },
        _=>return Err(Error::Denied),
    }
    Ok(())
}
pub(super) fn read_back(home:&Path,item:&ConfirmationItem,expected:&str)->Result<()> {
    let section=text(item,if item.category==ConfirmationCategory::Policy {"name"} else {"section"})?;
    let wanted:toml::Table=expected.parse().map_err(|_|Error::Invalid)?;
    let actual:toml::Table=read_optional(&home.join("config.toml"))?.parse().map_err(|_|Error::Apply)?;
    if actual!=wanted {return Err(Error::Stale);}
    match section {
        "container"=>{
            let expected=crate::task_sandbox::settings::parse(&wanted).map_err(|_|Error::Apply)?;
            let (actual,task_fallback)=crate::task_sandbox::settings::load(home);
            let (scripts,script_fallback)=crate::task_sandbox::settings::load_for_scripts(home);
            if actual.map_err(|_|Error::Apply)?!=expected || scripts.map_err(|_|Error::Apply)?!=expected
                || task_fallback!=crate::task_sandbox::settings::when_unavailable(&wanted)
                || script_fallback!=crate::task_sandbox::settings::script_when_unavailable(&wanted) {return Err(Error::Apply);}
        },
        "computer_use"=>{
            if crate::computer_use_image::load(home).map_err(|_|Error::Apply)?!=crate::computer_use_image::parse(&wanted).map_err(|_|Error::Apply)?
                || crate::computer_workspaces::config::load(home).map_err(|_|Error::Apply)?!=crate::computer_workspaces::config::parse(&wanted).map_err(|_|Error::Apply)? {return Err(Error::Apply);}
        },
        "provenance"=>{
            // Both CLI spawns and tool-loop construction call this same parser
            // on freshly read config; existing in-flight turns keep their policy.
            if crate::channel_reply::parse_provenance_settings(&actual)!=crate::channel_reply::parse_provenance_settings(&wanted) {return Err(Error::Apply);}
        },
        _=>return Err(Error::Denied),
    }
    Ok(())
}
#[cfg(test)] mod tests {
    use super::*;
    #[test]
    #[cfg(unix)]
    fn migration_authority_read_per_use_policy_inert_until_publish_and_compensates() {
        let temp=tempfile::tempdir().unwrap();std::fs::write(temp.path().join("config.toml"),"").unwrap();
        let request=duduclaw_import::ImportRequest {version:1,migration_id:duduclaw_migration::ids::RequestId([1;16]),employee:None,source_id:"old-config".into(),kind:duduclaw_import::SourceKind::AuthorityToml,title:"config.toml".into(),legacy_agent:None,has_wal:false,blob_handle:None};
        let old=b"[provenance]\npolicy='enforce'\nsensitive_tools=['send_to_agent']\n";
        let proposal=duduclaw_import::authority::extract(&request,&old[..],None).unwrap().into_iter().find(|p|p.category==ConfirmationCategory::ConfigurationSection).unwrap();
        let item=ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:duduclaw_migration::ids::RequestId([4;16]),revision:1},proposal_digest:Sha256Digest([5;32])},category:proposal.category,status:ConfirmationStatus::Applying,high_risk:true,fields:proposal.fields};
        let mut a=GatewayAuthority::new(temp.path().into()).unwrap();a.stage(&item).unwrap();assert_eq!(std::fs::read_to_string(temp.path().join("config.toml")).unwrap(),"");
        a.publish(&item).unwrap();a.read_back(&item).unwrap();a.disable_and_read_back(&item).unwrap();assert_eq!(std::fs::read_to_string(temp.path().join("config.toml")).unwrap(),"");
    }
    #[test] fn migration_authority_known_policy_rejects_ignored_keys_and_cached_sections() {
        for (section,content) in [("provenance","[provenance]\npolicy='paranoid'"),("container","[container]\nprivileged=true"),("computer_use","[computer_use.workspaces]\nenabeld=true")] {
            assert!(validate(section,&content.parse().unwrap()).is_err());
        }
        assert!(!section_supported("dispatch"));assert!(!section_supported("channels"));
    }
}
