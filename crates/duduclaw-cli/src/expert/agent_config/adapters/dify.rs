//! Dify app DSL (`*.yml`, `kind: app`; keys from
//! `api/services/app_dsl_service.py`, DSL 0.x).
//!
//! | DSL                                        | DuDuClaw                       |
//! |--------------------------------------------|--------------------------------|
//! | `app.name` / `app.icon` / `app.description`| display name / icon / IDENTITY |
//! | `model_config.pre_prompt` (chat apps)      | INSTRUCTIONS.md                |
//! | first `llm` node system prompt (workflows) | INSTRUCTIONS.md + "需重建" note |
//! | `model_config.opening_statement`           | wiki knowledge page            |
//! | `model_config.model`                       | model (Claude ids only)        |
//! | dataset references                         | listed, content not in export  |
//! | agent tools, plugin `dependencies`         | listed, not imported           |
//! | `environment_variables` (secret)           | never imported                 |
//! | workflow graph                             | not converted; file archived   |

use std::path::Path;

use super::super::detect::{ConfigKind, dify_dsl_file};
use super::super::ir::{AgentDraft, Doc, ImportBundle, Unmapped};
use super::super::md;
use super::bundle;

pub fn read(root: &Path) -> Result<ImportBundle, String> {
    let path = dify_dsl_file(root).ok_or_else(|| "找不到 Dify 應用匯出檔".to_string())?;
    let rel = md::rel(root, &path);
    let text = md::read_text(&path).ok_or_else(|| format!("{rel} 無法讀取"))?;
    let v: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| format!("{rel} 不是合法的 YAML：{e}"))?;
    let mut b = bundle(ConfigKind::Dify);
    let s = |val: Option<&serde_yaml::Value>| -> Option<String> {
        val.and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
    };
    let app = v.get("app");
    let name = s(app.and_then(|a| a.get("name"))).unwrap_or_else(|| "dify-app".into());
    let mode = s(app.and_then(|a| a.get("mode"))).unwrap_or_default();
    b.version = s(v.get("version")).unwrap_or_default();

    let mut a = AgentDraft {
        ref_id: "app".into(),
        id_hint: name.clone(),
        display_name: name.clone(),
        description: s(app.and_then(|a| a.get("description"))).unwrap_or_default(),
        emoji: s(app.and_then(|a| a.get("icon"))).filter(|i| md::is_emoji_token(i)),
        ..Default::default()
    };
    if !a.description.is_empty() {
        a.identity = Some(Doc::new(
            rel.clone(),
            format!("# {name}\n\n{}\n", a.description),
        ));
    }

    let mc = v.get("model_config");
    if let Some(p) = s(mc.and_then(|m| m.get("pre_prompt"))) {
        a.instructions = Some(Doc::new(format!("{rel}#model_config.pre_prompt"), p));
    } else if let Some(p) = workflow_llm_prompt(&v) {
        a.instructions = Some(Doc::new(format!("{rel}#workflow.llm"), p));
    }
    if let Some(open) = s(mc.and_then(|m| m.get("opening_statement"))) {
        a.knowledge.push(Doc::new(
            format!("{rel}#opening_statement"),
            format!("# 開場白\n\n{open}\n"),
        ));
    }
    a.model = s(mc.and_then(|m| m.get("model")).and_then(|m| m.get("name")))
        .or_else(|| s(workflow_llm_model(&v).as_ref()));

    if matches!(mode.as_str(), "workflow" | "advanced-chat") {
        b.warnings.push(format!(
            "「{name}」是 Dify 工作流程應用（{mode}）；流程圖不會自動轉換，只取第一個 LLM 節點的提示詞，請在 DuDuClaw 重建流程"
        ));
    }
    for ds in dataset_ids(mc) {
        a.skipped.push((
            format!("知識庫 {ds}"),
            "Dify 匯出檔不含知識庫內容，請在 DuDuClaw 重新上傳文件".into(),
        ));
    }
    for t in agent_tools(mc) {
        a.skipped.push((
            format!("工具 {t}"),
            "Dify 工具未匯入，需要時請在 DuDuClaw 另行設定".into(),
        ));
    }
    if let Some(deps) = v
        .get("dependencies")
        .and_then(|d| d.as_sequence())
        .filter(|d| !d.is_empty())
    {
        b.warnings
            .push(format!("應用依賴 {} 個 Dify 外掛，未匯入", deps.len()));
    }
    let secret_vars = v
        .get("workflow")
        .and_then(|w| w.get("environment_variables"))
        .and_then(|e| e.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter(|x| s(x.get("value_type")).as_deref() == Some("secret"))
                .count()
        })
        .unwrap_or(0);
    if secret_vars > 0 {
        b.warnings.push(format!(
            "{secret_vars} 個機密環境變數不會匯入，請在 DuDuClaw 重新設定"
        ));
    }

    if secret_vars == 0 {
        b.unmapped.push(Unmapped {
            rel: rel.clone(),
            reason: "Dify 原始匯出檔備份（含無法轉換的流程與設定）".into(),
        });
    } else {
        b.warnings.push("匯出檔含機密環境變數，原檔不保存".into());
    }
    b.display_name = name.clone();
    b.description = a.description.clone();
    b.slug_hint = name;
    b.agents.push(a);
    Ok(b)
}

fn llm_nodes(v: &serde_yaml::Value) -> Vec<&serde_yaml::Value> {
    v.get("workflow")
        .and_then(|w| w.get("graph"))
        .and_then(|g| g.get("nodes"))
        .and_then(|n| n.as_sequence())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("data"))
                .filter(|d| d.get("type").and_then(|t| t.as_str()) == Some("llm"))
                .collect()
        })
        .unwrap_or_default()
}

fn workflow_llm_prompt(v: &serde_yaml::Value) -> Option<String> {
    let node = llm_nodes(v).into_iter().next()?;
    let tpl = node.get("prompt_template")?;
    if let Some(seq) = tpl.as_sequence() {
        return seq
            .iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            .and_then(|m| m.get("text"))
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .filter(|t| !t.trim().is_empty());
    }
    tpl.get("text").and_then(|t| t.as_str()).map(str::to_string)
}

fn workflow_llm_model(v: &serde_yaml::Value) -> Option<serde_yaml::Value> {
    llm_nodes(v)
        .into_iter()
        .next()?
        .get("model")?
        .get("name")
        .cloned()
}

fn dataset_ids(mc: Option<&serde_yaml::Value>) -> Vec<String> {
    mc.and_then(|m| m.get("dataset_configs"))
        .and_then(|d| d.get("datasets"))
        .and_then(|d| d.get("datasets"))
        .and_then(|d| d.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|x| {
                    x.get("dataset")
                        .and_then(|d| d.get("id"))
                        .and_then(|i| i.as_str())
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn agent_tools(mc: Option<&serde_yaml::Value>) -> Vec<String> {
    mc.and_then(|m| m.get("agent_mode"))
        .and_then(|a| a.get("tools"))
        .and_then(|t| t.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|t| {
                    t.get("tool_label")
                        .or_else(|| t.get("tool_name"))
                        .and_then(|x| x.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_app_prompt_model_datasets_and_tools() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("客服.yml"),
            r#"app:
  name: 門市客服
  mode: agent-chat
  icon: "🤖"
  description: 回答門市問題
kind: app
version: 0.3.0
model_config:
  pre_prompt: 你是門市客服，只回答營業時間與退換貨。
  opening_statement: 您好！
  model:
    provider: openai
    name: gpt-4o
  dataset_configs:
    datasets:
      datasets:
        - dataset:
            enabled: true
            id: ds-123
  agent_mode:
    tools:
      - tool_name: google_search
        tool_label: Google 搜尋
"#,
        )
        .unwrap();
        let b = read(d.path()).unwrap();
        let a = &b.agents[0];
        assert_eq!(a.display_name, "門市客服");
        assert_eq!(a.emoji.as_deref(), Some("🤖"));
        assert!(a.instructions.as_ref().unwrap().text.contains("退換貨"));
        assert_eq!(a.model.as_deref(), Some("gpt-4o"));
        assert_eq!(a.knowledge.len(), 1);
        assert!(a.skipped.iter().any(|(s, _)| s.contains("ds-123")));
        assert!(a.skipped.iter().any(|(s, _)| s.contains("Google 搜尋")));
        assert_eq!(b.unmapped.len(), 1, "the DSL itself is archived");
    }

    #[test]
    fn workflow_app_takes_first_llm_system_prompt_and_warns() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("flow.yml"),
            r#"app: {name: 報價流程, mode: workflow}
kind: app
version: 0.3.0
workflow:
  environment_variables:
    - {name: KEY, value_type: secret, value: sk-1}
  graph:
    nodes:
      - data: {type: start}
      - data:
          type: llm
          model: {name: claude-sonnet-4-6}
          prompt_template:
            - {role: system, text: 依報價單計算總價}
"#,
        )
        .unwrap();
        let b = read(d.path()).unwrap();
        let a = &b.agents[0];
        assert_eq!(a.instructions.as_ref().unwrap().text, "依報價單計算總價");
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!(b.warnings.iter().any(|w| w.contains("重建流程")));
        assert!(b.warnings.iter().any(|w| w.contains("機密環境變數")));
    }
}
