use super::*;
use std::collections::HashMap;

fn selected(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
}

#[test]
fn restricted_tools_and_selected_credentials_are_not_ambient() {
    let selected=selected(&[("OPENAI_API_KEY","selected"),("ANTHROPIC_API_KEY","foreign"),("LD_PRELOAD","evil"),("HOME","/real-home")]);
    let env=environment(RuntimeFamily::Codex,&selected);
    assert_eq!(env["OPENAI_API_KEY"],"selected");assert!(!env.contains_key("ANTHROPIC_API_KEY"));assert!(!env.contains_key("LD_PRELOAD"));assert_ne!(env["HOME"],"/real-home");
    let args=RuntimeFamily::Codex.argv("test",2,Path::new("/work"));assert!(args.contains(&"--ignore-user-config".into()));assert!(args.contains(&"web_search=\"disabled\"".into()));assert!(!args.contains(&"--add-dir".into()));
}

#[test]
fn every_family_parses_with_aliases_and_unknown_names_are_refused() {
    for (name, family, provider) in [("claude",RuntimeFamily::Claude,"anthropic"),("codex",RuntimeFamily::Codex,"openai"),
        ("gemini",RuntimeFamily::Gemini,"gemini"),("antigravity",RuntimeFamily::Antigravity,"gemini"),("agy",RuntimeFamily::Antigravity,"gemini"),
        ("grok",RuntimeFamily::Grok,"xai"),("openai-compat",RuntimeFamily::OpenAiCompat,"openai"),("openai_compat",RuntimeFamily::OpenAiCompat,"openai")] {
        let parsed=RuntimeFamily::parse(name).unwrap();
        assert_eq!(parsed,family);assert_eq!(parsed.provider(),provider);
        assert_eq!(RuntimeFamily::parse(parsed.name()).unwrap(),family,"canonical name round-trips");
    }
    assert!(matches!(RuntimeFamily::parse("Grok"),Err(AttemptInfraError::RuntimeUnsupported(_))));
    assert!(matches!(RuntimeFamily::parse("cursor"),Err(AttemptInfraError::RuntimeUnsupported(_))));
}

#[test]
fn argv_is_pinned_per_family() {
    let node=Path::new("/runs/r/ws");
    let codex=RuntimeFamily::Codex.argv("gpt-5.4",3,node);
    assert_eq!(&codex[..12],&["exec","--json","--strict-config","--ignore-user-config","--ignore-rules","--ephemeral",
        "--skip-git-repo-check","--dangerously-bypass-approvals-and-sandbox","--model","gpt-5.4","--cd","/runs/r/ws"].map(String::from));
    let settings=codex.windows(2).filter(|p|p[0]=="-c").map(|p|p[1].as_str()).collect::<Vec<_>>();
    assert_eq!(settings,["approval_policy=\"never\"","mcp_servers={}","web_search=\"disabled\"","features.multi_agent=false",
        "features.hooks=false","features.plugins=false","features.memories=false","project_doc_max_bytes=0","features.apps=false",
        "check_for_update_on_startup=false","analytics.enabled=false"]);
    assert_eq!(codex.last().map(String::as_str),Some("-"));
    let grok=RuntimeFamily::Grok.argv("grok-4.7",4,node);
    assert_eq!(grok,["--prompt-file","/dudu-runtime/prompt.txt","--output-format","streaming-messages-json","--cwd","/runs/r/ws",
        "-m","grok-4.7","--max-turns","4","--permission-mode","bypassPermissions",
        "--tools","run_terminal_command,read_file,search_replace,list_dir,grep,write",
        "--disallowed-tools","todo_write,monitor,search_tool,use_tool,workflow,enter_plan_mode,exit_plan_mode,ask_user_question,send_feedback,image_gen,image_edit,image_to_video,reference_to_video,spawn_subagent,scheduler_create,scheduler_delete,scheduler_list,kill_command_or_subagent,get_command_or_subagent_output",
        "--disable-web-search","--no-subagents","--no-plan","--no-auto-update"].map(String::from));
    assert!(!grok.iter().any(|a|a=="--trust"),"workspace hooks must not load");
    let agy=RuntimeFamily::Antigravity.argv("gemini-3.8-flash",4,node);
    assert_eq!(agy,["--print","","--input-format","stream-json","--output-format","stream-json",
        "--dangerously-skip-permissions","--disable-slash-commands","--model","gemini-3.8-flash"].map(String::from));
    assert!(RuntimeFamily::Claude.argv("m",2,node).contains(&"--safe-mode".into()));
}

#[test]
fn stdin_and_runtime_files_per_family() {
    let prompt="讀 solution.py \"quoted\"\nnext";
    assert!(stdin_payload(RuntimeFamily::Grok,prompt).is_empty());
    let line=stdin_payload(RuntimeFamily::Antigravity,prompt);
    assert_eq!(line.last(),Some(&b'\n'));assert_eq!(line.iter().filter(|b|**b==b'\n').count(),1,"exactly one JSON line");
    let parsed:Value=serde_json::from_slice(&line).unwrap();
    assert_eq!(parsed,json!({"event":"user","message":{"content":prompt}}));
    for family in [RuntimeFamily::Claude,RuntimeFamily::Codex,RuntimeFamily::Gemini,RuntimeFamily::OpenAiCompat] {
        assert_eq!(stdin_payload(family,prompt),prompt.as_bytes());
        assert!(runtime_files(family,Path::new("/w"),1,prompt).is_empty());
    }
    assert_eq!(runtime_files(RuntimeFamily::Grok,Path::new("/w"),1,prompt),vec![("prompt.txt".to_string(),prompt.as_bytes().to_vec())]);
    let files=runtime_files(RuntimeFamily::Antigravity,Path::new("/runs/r/ws"),1,prompt).into_iter().collect::<BTreeMap<_,_>>();
    assert_eq!(files.keys().map(String::as_str).collect::<Vec<_>>(),["agy_tool_guard.py","home-seed/.gemini/antigravity-cli/settings.json","home-seed/.gemini/config/hooks.json"]);
    let settings:Value=serde_json::from_slice(&files["home-seed/.gemini/antigravity-cli/settings.json"]).unwrap();
    assert_eq!(settings,json!({"modelProvider":"gemini","trustedWorkspaces":["/runs/r/ws"]}));
    let hooks:Value=serde_json::from_slice(&files["home-seed/.gemini/config/hooks.json"]).unwrap();
    assert_eq!(hooks["dudu-tool-surface"]["PreToolUse"][0]["matcher"],"*");
    assert_eq!(hooks["dudu-tool-surface"]["PreToolUse"][0]["hooks"][0]["command"],"python3 -I -S -B /dudu-runtime/agy_tool_guard.py");
    let script=String::from_utf8(files["agy_tool_guard.py"].clone()).unwrap();
    assert!(!script.contains("__DUDU_ALLOWED_TOOLS_JSON__"));
    assert!(script.contains(&json!(super::super::attempt_guard::ANTIGRAVITY_TOOLS).to_string()),"allowlist rendered from the guard constant");
}

#[cfg(unix)]
#[test]
fn agy_hook_script_allows_only_the_allowlist_and_fails_closed() {
    use std::io::Write;
    let dir=tempfile::tempdir().unwrap();
    let script=dir.path().join("guard.py");std::fs::write(&script,agy_guard_script()).unwrap();
    let broken=dir.path().join("broken.py");std::fs::write(&broken,AGY_GUARD_TEMPLATE).unwrap();
    let decide=|path:&Path,input:&str|->String {
        let mut child=std::process::Command::new("python3").args(["-I","-S","-B"]).arg(path)
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out=child.wait_with_output().unwrap();
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["decision"].as_str().unwrap().to_owned()
    };
    assert_eq!(decide(&script,r#"{"toolCall":{"name":"run_command","args":{}},"modelName":"m"}"#),"allow");
    for input in [r#"{"toolCall":{"name":"search_web"}}"#,r#"{"toolCall":{"name":["run_command"]}}"#,"not json","",r#"{"toolCall":null}"#] {
        assert_eq!(decide(&script,input),"deny","{input}");
    }
    assert_eq!(decide(&broken,r#"{"toolCall":{"name":"run_command"}}"#),"deny","an unrendered allowlist denies");
}

#[test]
fn credentials_do_not_cross_families_and_codex_gets_both_key_names() {
    let all=selected(&[("OPENAI_API_KEY","o"),("ANTHROPIC_API_KEY","a"),("CLAUDE_CODE_OAUTH_TOKEN","t"),("GEMINI_API_KEY","g"),
        ("GOOGLE_API_KEY","gg"),("XAI_API_KEY","x"),("CODEX_API_KEY","ambient"),("GROK_HOME","/host/.grok")]);
    let secrets=["OPENAI_API_KEY","CODEX_API_KEY","ANTHROPIC_API_KEY","CLAUDE_CODE_OAUTH_TOKEN","GEMINI_API_KEY","GOOGLE_API_KEY","XAI_API_KEY"];
    let expect:[(RuntimeFamily,&[&str]);6]=[(RuntimeFamily::Claude,&["ANTHROPIC_API_KEY","CLAUDE_CODE_OAUTH_TOKEN"]),
        (RuntimeFamily::Codex,&["CODEX_API_KEY","OPENAI_API_KEY"]),(RuntimeFamily::Gemini,&["GEMINI_API_KEY","GOOGLE_API_KEY"]),
        (RuntimeFamily::Antigravity,&["GEMINI_API_KEY","GOOGLE_API_KEY"]),(RuntimeFamily::Grok,&["XAI_API_KEY"]),(RuntimeFamily::OpenAiCompat,&["OPENAI_API_KEY"])];
    for (family,keys) in expect {
        let env=environment(family,&all);
        let present=secrets.iter().copied().filter(|k|env.contains_key(*k)).collect::<BTreeSet<_>>();
        assert_eq!(present,keys.iter().copied().collect::<BTreeSet<_>>(),"{family:?}");
        assert!(has_credentials(family,&env,None));
        assert!(!has_credentials(family,&environment(family,&HashMap::new()),None));
    }
    assert_eq!(environment(RuntimeFamily::Codex,&all)["CODEX_API_KEY"],"o","derived from the selected key, never ambient");
    let grok=environment(RuntimeFamily::Grok,&all);
    assert_eq!(grok["GROK_HOME"],"/tmp/dudu-private/home/.grok");assert_eq!(grok["GROK_DISABLE_AUTOUPDATER"],"1");
    assert_eq!(grok["GROK_SUBAGENTS"],"0");assert_eq!(grok["GROK_MEMORY"],"0");
    assert!(!environment(RuntimeFamily::Claude,&all).contains_key("GROK_HOME"));
}

#[test]
fn credential_document_is_validated_and_only_for_codex_and_grok() {
    let doc=r#"{ "auth_mode": "chatgpt",
        "tokens": {"refresh_token": "r"} }"#;
    let line=credential_document(RuntimeFamily::Codex,Some(doc)).unwrap().unwrap();
    assert!(!line.contains('\n'));assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["auth_mode"],"chatgpt");
    assert!(credential_document(RuntimeFamily::Grok,Some(doc)).unwrap().is_some());
    for family in [RuntimeFamily::Claude,RuntimeFamily::Gemini,RuntimeFamily::Antigravity,RuntimeFamily::OpenAiCompat] {
        assert_eq!(credential_document(family,Some(doc)),Ok(None));assert_eq!(credential_destination(family),None);
        assert!(!has_credentials(family,&BTreeMap::new(),Some(&line)),"a document is not a credential for {family:?}");
    }
    assert_eq!(credential_destination(RuntimeFamily::Codex),Some(".codex/auth.json"));
    assert_eq!(credential_destination(RuntimeFamily::Grok),Some(".grok/auth.json"));
    assert_eq!(credential_document(RuntimeFamily::Codex,None),Ok(None));
    let oversized=format!("{{\"k\":\"{}\"}}","x".repeat(CREDENTIAL_DOC_MAX_BYTES));
    for invalid in ["not json","[1,2]","\"string\"","{}","42",oversized.as_str()] {
        assert_eq!(credential_document(RuntimeFamily::Codex,Some(invalid)),Err(AttemptInfraError::NoAccount),"{}",duduclaw_core::truncate_bytes(invalid,20));
    }
    assert!(has_credentials(RuntimeFamily::Codex,&BTreeMap::new(),Some(&line)));
    assert!(has_credentials(RuntimeFamily::Grok,&BTreeMap::new(),Some(&line)));
    assert!(!has_credentials(RuntimeFamily::Grok,&BTreeMap::new(),Some("")));
}

#[test]
fn native_usage_is_known_only_when_both_counts_arrive_and_model_is_not_invented() {
    let mut adapter=StreamAdapter::default();
    let event=adapter.normalize(RuntimeFamily::Codex,json!({"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":5,"output_tokens":3}}));
    assert_eq!(event["usage"]["input_tokens"],7);assert_eq!(event["usage"]["cache_read_input_tokens"],5);assert!(event.get("model").is_none());
    let unknown=adapter.normalize(RuntimeFamily::Codex,json!({"type":"turn.completed","usage":{"input_tokens":12}}));assert!(unknown["usage"].is_null());
    assert!(adapter.observed_model().is_none());
    assert!(compatible_endpoint("https://user:secret@example.test/v1").is_err());assert!(compatible_endpoint("http://example.test/v1").is_err());assert!(compatible_endpoint("http://127.0.0.1:9999/v1").is_ok());
}

#[test]
fn contradictory_codex_cache_usage_is_unknown_instead_of_fabricated() {
    let mut adapter=StreamAdapter::default();
    let event=adapter.normalize(RuntimeFamily::Codex,json!({"type":"turn.completed","usage":{"input_tokens":5,"cached_input_tokens":8,"output_tokens":3}}));
    assert!(event["usage"].is_null(),"cached tokens cannot exceed total input");
    let summary=super::super::agent_spawn::parse_stream(&event.to_string());
    assert!(summary.usage.is_none());
    assert_eq!(super::super::agent_spawn::call_cost("gpt-5.4",&summary).source,super::super::tree::CostSource::Unknown);
}

fn agy_step(index:u64,state:&str,step_type:&str,extra:Value)->Value {
    let mut step=json!({"conversation_id":"c","step_index":index,"state":state,"step_type":step_type});
    for (k,v) in extra.as_object().unwrap() { step[k]=v.clone(); }
    json!({"event":"step_update","step_update":step})
}
fn agy_usage(input:u64,output:u64,cache:u64)->Value { json!({"usage":{"input_tokens":input,"output_tokens":output,"thinking_tokens":1,"cache_read_tokens":cache,"total_tokens":input+output}}) }
fn transcript(family:RuntimeFamily,adapter:&mut StreamAdapter,events:Vec<Value>)->String {
    events.into_iter().map(|e|adapter.normalize(family,e).to_string()+"\n").collect()
}

#[test]
fn antigravity_steps_sum_usage_and_the_result_event_is_authoritative() {
    // Minimised from runtime-parity/spot/agy.ndjson (agy 1.2.14): the final
    // result usage equals the per-step sum.
    let events=vec![json!({"event":"init","conversation_id":"c","init":{"cwd":"/w","tools":["run_command"],"model":"gemini-3.8-flash-high","permission_mode":"always-proceed"}}),
        agy_step(0,"DONE","user_input",json!({})),agy_step(1,"DONE","agent_response",agy_usage(12875,1166,0)),
        agy_step(2,"ACTIVE","tool",json!({"tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"ls"}}})),
        agy_step(2,"DONE","tool",json!({"tool_name":"run_command"})),
        agy_step(3,"ACTIVE","agent_response",json!({"text_delta":"DONE"})),
        agy_step(3,"DONE","agent_response",{let mut u=agy_usage(2472,137,20326);u["text_delta"]=json!("\n");u}),
        agy_step(3,"DONE","agent_response",agy_usage(2472,137,20326)),
        json!({"event":"result","result":{"status":"SUCCESS","response":"DONE\n","num_turns":1,"usage":{"input_tokens":15347,"output_tokens":1303,"thinking_tokens":2,"cache_read_tokens":20326,"total_tokens":16650}}})];
    let mut adapter=StreamAdapter::default();
    let text=transcript(RuntimeFamily::Antigravity,&mut adapter,events);
    let lines=text.lines().map(|l|serde_json::from_str::<Value>(l).unwrap()).collect::<Vec<_>>();
    assert_eq!(lines[2]["message"]["id"],"step-1");assert_eq!(lines[2]["message"]["usage"]["input_tokens"],12875);
    assert_eq!(lines[2]["message"]["model"],"gemini-3.8-flash-high");
    assert_eq!(lines[3]["message"]["content"][0],json!({"type":"tool_use","name":"run_command"}));
    assert_eq!(lines[6]["message"]["content"][0]["text"],"DONE\n");assert_eq!(lines[6]["message"]["usage"]["cache_read_input_tokens"],20326);
    assert_eq!(lines[7]["type"],"system","a repeated DONE is not counted twice");
    let summary=super::super::agent_spawn::parse_stream(&text);
    assert!(summary.complete);assert_eq!(summary.final_text.as_deref(),Some("DONE\n"));
    let usage=summary.usage.unwrap();assert_eq!((usage.input_tokens,usage.output_tokens,usage.cache_read_tokens),(15347,1303,20326));
    assert_eq!(summary.model.as_deref(),Some("gemini-3.8-flash-high"));
    assert_eq!(adapter.observed_model(),Some("gemini-3.8-flash-high"));
    let synthetic=adapter.synthetic_result();
    assert_eq!(synthetic["usage"],json!({"input_tokens":15347,"output_tokens":1303,"cache_read_input_tokens":20326}));
}

#[test]
fn antigravity_step_limit_synthetic_result_and_error_paths() {
    let mut adapter=StreamAdapter::default();
    let mut text=transcript(RuntimeFamily::Antigravity,&mut adapter,vec![
        json!({"event":"init","init":{"cwd":"/w","tools":[]}}),
        agy_step(1,"DONE","agent_response",agy_usage(100,10,5)),
        agy_step(2,"ACTIVE","tool",json!({"tool_name":"view_file"})),
        agy_step(3,"ACTIVE","agent_response",json!({"text_delta":"partial"})),
        agy_step(3,"DONE","agent_response",agy_usage(200,20,0))]);
    text.push_str(&adapter.synthetic_result().to_string());text.push('\n');
    let summary=super::super::agent_spawn::parse_stream(&text);
    assert!(summary.complete);assert_eq!(summary.final_text.as_deref(),Some("partial"));
    let usage=summary.usage.unwrap();assert_eq!((usage.input_tokens,usage.output_tokens,usage.cache_read_tokens),(300,30,5));
    assert!(summary.model.is_none(),"no init.model, no invented model");
    // A generation without usage makes the synthetic total unknown.
    let mut partial=StreamAdapter::default();
    transcript(RuntimeFamily::Antigravity,&mut partial,vec![agy_step(1,"DONE","agent_response",agy_usage(1,1,0)),agy_step(3,"DONE","agent_response",json!({}))]);
    assert!(partial.synthetic_result()["usage"].is_null());
    assert!(StreamAdapter::default().synthetic_result()["usage"].is_null());
    // Fake-key container probe: error_message step, then result status ERROR.
    let mut failed=StreamAdapter::default();
    let error=failed.normalize(RuntimeFamily::Antigravity,agy_step(1,"DONE","error_message",json!({"error":"API key not valid"})));
    assert_eq!(error["type"],"error");assert!(error["message"].get("conversation_id").is_none());
    let result=failed.normalize(RuntimeFamily::Antigravity,json!({"event":"result","result":{"status":"ERROR","response":"","error":"agent executor error: API key not valid."}}));
    assert_eq!(result["is_error"],true);assert!(result["usage"].is_null());
}

#[test]
fn grok_model_is_adopted_only_when_well_formed() {
    let mut adapter=StreamAdapter::default();
    let ok=adapter.normalize(RuntimeFamily::Grok,json!({"type":"assistant","message":{"id":"msg_0","model":"grok-4.7","content":[]}}));
    assert_eq!(ok["message"]["model"],"grok-4.7");assert_eq!(adapter.observed_model(),Some("grok-4.7"));
    for bad in ["unknown","",&"g".repeat(65),"grok 4","grok/../x","模型","<synthetic>"] {
        let mut adapter=StreamAdapter::default();
        let event=adapter.normalize(RuntimeFamily::Grok,json!({"type":"assistant","message":{"model":bad,"content":[]}}));
        assert!(event["message"].get("model").is_none(),"{bad}");assert!(adapter.observed_model().is_none());
    }
    assert_eq!(observed_model_id("grok-4.7-build:2026.10_a"),Some("grok-4.7-build:2026.10_a"));
    assert_eq!(observed_model_id(&"m".repeat(64)).map(str::len),Some(64));
    // Grok's result passes through; the CLI's own bill is reported.
    let result=adapter.normalize(RuntimeFamily::Grok,json!({"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":2,"total_cost_usd":0.03283448,"usage":{"input_tokens":35627,"output_tokens":817,"cache_read_input_tokens":40832,"cache_creation_input_tokens":0}}));
    let summary=super::super::agent_spawn::parse_stream(&result.to_string());
    assert_eq!(super::super::agent_spawn::call_cost("grok-4.7",&summary).source,super::super::tree::CostSource::Reported);
}
