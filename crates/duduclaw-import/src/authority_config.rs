//! Explicit source readers for settings consumed by normal read-on-use policy
//! parsers. Unknown fields never become actionable through partial extraction.
use super::*;

fn keys(value:&Value,allowed:&[&str])->bool {
    value.as_object().is_some_and(|m|m.keys().all(|k|allowed.contains(&k.as_str())))
}
fn scalar(value:&Value)->bool {matches!(value,Value::String(_)|Value::Number(_)|Value::Bool(_))}
fn flat(value:&Value,allowed:&[&str])->bool {
    keys(value,allowed) && value.as_object().is_some_and(|m|m.values().all(scalar))
}
pub(super) fn reviewed_section(name:&str,value:&Value)->bool {
    match name {
        "delegation"=>keys(value,&["policy","pairs"]),
        "container"=>keys(value,&["sandbox"]) && value.get("sandbox").is_some_and(|sandbox| {
            keys(sandbox,&["image","executables","memory_bytes","pids","cpu_millis","tmp_bytes","workspace_bytes","max_turns","when_unavailable","script_when_unavailable"])
                && sandbox.as_object().is_some_and(|m|m.iter().all(|(key,v)|if key=="executables" {
                    flat(v,&["claude","codex","grok","openai-compat"])
                }else{scalar(v)}))
        }),
        "computer_use"=>keys(value,&["image","workspaces"]) && value.as_object().is_some_and(|m|m.iter().all(|(key,v)|if key=="workspaces" {
            flat(v,&["enabled","max_per_agent","max_bytes","max_files","retention_days","min_free_bytes","admin_approval_minutes"])
        }else{v.is_string()})),
        "provenance"=>keys(value,&["policy","sensitive_tools"]) && value.as_object().is_some_and(|m|m.iter().all(|(key,v)|if key=="policy" {
            matches!(v.as_str(),Some("off"|"warn"|"enforce"))
        }else{v.as_array().is_some_and(|a|a.len()<=256 && a.iter().all(|v|v.as_str().is_some_and(|s|!s.is_empty()&&s.len()<=128&&s.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'))))})),
        _=>false,
    }
}
fn source_id(value:&str)->bool {
    !value.is_empty() && value.len()<=64 && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'_')
}
pub(super) fn agent_grants(settings:&mut Value,agent:&str,b:&mut Builder<'_>)->Result<()> {
    if let Some(capabilities)=settings.get_mut("capabilities").and_then(Value::as_object_mut) {
        for name in ["action_rules", "trusted_read_hint_servers"] {
            if let Some(value)=capabilities.remove(name) {
                // These are separate authority decisions, never hidden inside
                // an employee allocation. Their normal apply adapter is pending.
                let known=if name=="action_rules" {
                    value.as_array().is_some_and(|rules| rules.len()<=256 && rules.iter().all(|rule| {
                        keys(rule,&["effect","tool","verdict"])
                            && matches!(rule.get("verdict").and_then(Value::as_str),Some("allow"|"ask"|"block"))
                            && (rule.get("effect").is_some() != rule.get("tool").is_some())
                            && rule.as_object().is_some_and(|m|m.values().all(|v|v.as_str().is_some_and(|s|!s.is_empty()&&s.len()<=128&&!s.chars().any(char::is_control))))
                    }))
                }else{value.as_array().is_some_and(|servers|servers.len()<=256 && servers.iter().all(|v|v.as_str().is_some_and(|s|!s.is_empty()&&s.len()<=128&&s.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'_'|b'-'|b'.')))))};
                let rules=if known {toml::to_string(&serde_json::json!({name:value})).map_err(|_|Error::Content)?}else{String::new()};
                b.push(C::Policy,vec![field("employee_id",agent),field("name",name),field("rules",rules),field("requires_reentry","true")],None)?;
            }
        }
    }
    if let Some(sources)=settings.get_mut("capabilities").and_then(Value::as_object_mut).and_then(|m|m.remove("db_sources")) {
        let mut unique=std::collections::BTreeSet::new();
        for source in sources.as_array().ok_or(Error::Content)? {
            let source=source.as_str().filter(|s|source_id(s)).ok_or(Error::Content)?;
            if unique.insert(source) {b.push(C::DataSourceAuthorization,vec![field("name",source),field("employee_id",agent),field("grants","read_only")],None)?;}
        }
    }
    if let Some(budget)=settings.as_object_mut().and_then(|m|m.remove("budget")) {
        let budget=budget.as_object().ok_or(Error::Content)?;
        let known=budget.keys().all(|k|matches!(k.as_str(),"daily_cap_cents"|"monthly_limit_cents"|"hard_stop"|"when_unavailable"|"warn_threshold_percent"));
        if !known {
            b.push(C::Budget,vec![field("name",agent),field("requires_reentry","true")],None)?;
        }else {
            for (key,period) in [("daily_cap_cents","day"),("monthly_limit_cents","month")] {
                if let Some(amount)=budget.get(key) {
                    let amount=amount.as_i64().filter(|n|*n>=0).ok_or(Error::Content)?;
                    let mut fields=vec![field("name",agent),field("period",period),field("amount_cents",amount.to_string())];
                    // Zero was an unlimited legacy budget. It cannot establish
                    // an enforceable cap in the fresh authority store.
                    if amount==0 {fields.push(field("requires_reentry","true"));}
                    b.push(C::Budget,fields,None)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(title:&str)->ImportRequest {ImportRequest {version:1,migration_id:RequestId([8;16]),employee:None,source_id:"old:config".into(),kind:SourceKind::AuthorityToml,title:title.into(),legacy_agent:Some("sales".into()),has_wal:false,blob_handle:None}}
    fn text<'a>(p:&'a AuthorityProposal,name:&str)->&'a str {match &p.fields.iter().find(|f|f.name==name).unwrap().value {ProposalValue::Text{value}=>value,_=>panic!("text")}}
    #[test]
    fn authority_agent_grants_and_budget_require_separate_confirmation() {
        let raw=b"runtime='claude'\n[capabilities]\ndb_sources=['crm_pg','crm_pg','orders']\n[budget]\ndaily_cap_cents=400\nmonthly_limit_cents=4000\nwarn_threshold_percent=80\nhard_stop=false\nwhen_unavailable='allow'\n";
        let p=extract(&request("agent.toml"),raw.as_slice(),None).unwrap();
        assert_eq!(p.len(),5);
        assert_eq!(p.iter().filter(|p|p.category==C::DataSourceAuthorization).count(),2);
        for grant in p.iter().filter(|p|p.category==C::DataSourceAuthorization) {assert_eq!(text(grant,"grants"),"read_only");assert_eq!(grant.fields.len(),3);}
        let budgets:Vec<_>=p.iter().filter(|p|p.category==C::Budget).collect();assert_eq!(budgets.len(),2);
        assert_eq!(text(budgets[0],"amount_cents"),"400");assert_eq!(text(budgets[1],"period"),"month");
        let employee=p.iter().find(|p|p.category==C::Employee).unwrap();let settings:Value=serde_json::from_str(text(employee,"settings")).unwrap();
        assert!(settings.get("budget").is_none());assert!(settings["capabilities"].get("db_sources").is_none());
    }
    #[test]
    fn authority_agent_action_rules_are_separate_inactive_policy_proposals() {
        let raw=b"[capabilities]\naction_rules=[{effect='send',verdict='block'}]\ntrusted_read_hint_servers=['crm']\n";
        let proposals=extract(&request("agent.toml"),raw.as_slice(),None).unwrap();
        assert_eq!(proposals.len(),3);
        let policies:Vec<_>=proposals.iter().filter(|p|p.category==C::Policy).collect();
        assert_eq!(policies.len(),2);
        assert!(policies.iter().all(|p|text(p,"employee_id")=="sales"&&text(p,"requires_reentry")=="true"));
        let employee=proposals.iter().find(|p|p.category==C::Employee).unwrap();
        let settings:Value=serde_json::from_str(text(employee,"settings")).unwrap();
        assert!(settings["capabilities"].get("action_rules").is_none());
        assert!(settings["capabilities"].get("trusted_read_hint_servers").is_none());
        let malformed=b"[capabilities]\naction_rules=[{effect='send',verdict='block',future_secret='SECRET-CANARY'}]\n";
        let proposals=extract(&request("agent.toml"),malformed.as_slice(),None).unwrap();
        assert_eq!(text(&proposals[0],"rules"),"");
        assert!(!serde_json::to_string(&proposals).unwrap().contains("SECRET-CANARY"));
    }
    #[test]
    fn authority_agent_invalid_grants_reject_and_unknown_budget_requires_reentry() {
        for raw in ["[capabilities]\ndb_sources=['../secret']", "[capabilities]\ndb_sources=['CRM']", "[budget]\ndaily_cap_cents=-1"] {
            assert!(extract(&request("agent.toml"),raw.as_bytes(),None).is_err());
        }
        let p=extract(&request("agent.toml"),b"[budget]\ndaily_cap_cents=1\nunknown='SECRET'".as_slice(),None).unwrap();
        let budget=p.iter().find(|p|p.category==C::Budget).unwrap();assert_eq!(text(budget,"requires_reentry"),"true");
        assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
    }
    #[test]
    fn authority_reviewed_config_sections_preserve_only_known_shapes() {
        let raw=b"[container.sandbox]\nimage='safe:1'\npids=128\n[container.sandbox.executables]\nclaude='/usr/bin/claude'\n[computer_use]\nimage='safe:2'\n[computer_use.workspaces]\nenabled=true\nmax_files=50\n[provenance]\npolicy='enforce'\nsensitive_tools=['send_to_agent']\n";
        let p=extract(&request("config.toml"),raw.as_slice(),None).unwrap();assert_eq!(p.len(),3);
        assert!(p.iter().all(|p|p.category==C::ConfigurationSection&&!p.fields.iter().any(|f|f.name=="requires_reentry")));
        assert!(p.iter().any(|p|text(p,"settings").contains("send_to_agent")));
    }
    #[test]
    fn authority_unknown_config_semantics_and_credentials_are_withheld() {
        for raw in ["[container.sandbox]\nfuture='SECRET'", "[container.sandbox.executables]\nfuture='SECRET'", "[computer_use.profiles]\nroot='SECRET'", "[provenance]\nfuture='SECRET'", "[provenance]\nsensitive_tools=[{token='SECRET'}]"] {
            let p=extract(&request("config.toml"),raw.as_bytes(),None).unwrap();assert_eq!(text(&p[0],"requires_reentry"),"true");
            assert!(!serde_json::to_string(&p).unwrap().contains("SECRET"));
        }
    }
}
