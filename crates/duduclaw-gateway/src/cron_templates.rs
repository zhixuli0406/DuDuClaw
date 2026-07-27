//! Preset "office scheduling" templates for the dashboard's routine
//! creation UI. Each template ships a friendly zh-TW name, a suggested
//! cron expression, and a prompt body whose customisable spots are wrapped
//! in 〈…〉 angle-bracket placeholders so the user can fill in their own
//! mailbox / team / data source before saving.
//!
//! These are pure constants (no I/O). The dashboard fetches them via the
//! `cron.templates` RPC and prefills the create dialog; nothing here writes
//! to the store. A unit test asserts every template's cron expression parses
//! so a typo can never ship a template that the scheduler would reject.

use serde::Serialize;
use serde_json::{json, Value};

/// One preset routine template surfaced in the dashboard.
///
/// `cron` is stored in the user-friendly 5-field form; the scheduler's
/// [`crate::cron_scheduler::normalise_cron`] promotes it to 6 fields at
/// load time exactly as it does for hand-typed expressions.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CronTemplate {
    /// Stable machine id (used as a React key / selection value).
    pub id: &'static str,
    /// Human-facing template name (zh-TW). Prefilled into the routine name.
    pub name: &'static str,
    /// Suggested cron expression (5-field, minute precision).
    pub cron: &'static str,
    /// One-line description of when it fires / what it does (zh-TW).
    pub description: &'static str,
    /// Prompt body prefilled into the task field. 〈…〉 marks spots the user
    /// should customise before saving.
    pub prompt: &'static str,
}

/// The built-in office scheduling templates, in display order.
///
/// Cron expressions are interpreted in the task's `cron_timezone` when the
/// user sets one on the create form, otherwise UTC — the suggested times
/// below read as local business hours and the create dialog exposes the
/// timezone control, so the prompt copy stays timezone-agnostic.
pub const OFFICE_TEMPLATES: &[CronTemplate] = &[
    CronTemplate {
        id: "daily-email-digest",
        name: "每日郵件摘要",
        cron: "0 9 * * *",
        description: "每天早上 09:00 彙整重要郵件",
        prompt: "請彙整〈信箱／收件匣〉在過去 24 小時收到的重要郵件，依主題分類後產出精簡摘要，並標出今天需要我親自回覆的項目。",
    },
    CronTemplate {
        id: "weekly-report",
        name: "週報彙整",
        cron: "0 17 * * 5",
        description: "每週五 17:00 產出可轉發的週報",
        prompt: "請彙整本週〈團隊／專案〉的進度：已完成事項、進行中事項、下週重點，以及待我裁決的議題，整理成一份可以直接轉發的週報。",
    },
    CronTemplate {
        id: "daily-revenue-report",
        name: "每日營收報表",
        cron: "0 10 * * *",
        description: "每天 10:00 產出一頁式營收日報",
        prompt: "請從〈資料來源，例如 Odoo／試算表〉拉取昨日營收數字，對比前一日與上週同一天，列出前五大商品與異常波動，產出一頁式營收日報。",
    },
    CronTemplate {
        id: "inventory-check-reminder",
        name: "庫存盤點提醒",
        cron: "0 8 * * 1",
        description: "每週一 08:00 檢查庫存水位",
        prompt: "請檢查〈倉庫／品項清單〉的庫存水位，列出低於安全庫存的品項與建議補貨數量，並提醒本週需要人工盤點的貨架。",
    },
    CronTemplate {
        id: "social-post-draft",
        name: "社群貼文排程草稿",
        cron: "0 14 * * *",
        description: "每天 14:00 草擬待審社群貼文",
        prompt: "請依〈品牌／主題〉的近期重點，草擬今天要發佈的社群貼文（含文案與建議發文時間），風格參考〈平台，例如 Threads／IG〉，最後交我審核再發佈。",
    },
];

/// Serialize the built-in templates into the JSON array the dashboard reads.
pub fn templates_json() -> Vec<Value> {
    OFFICE_TEMPLATES
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "name": t.name,
                "cron": t.cron,
                "description": t.description,
                "prompt": t.prompt,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_scheduler::normalise_cron;
    use std::collections::HashSet;

    #[test]
    fn every_template_cron_parses() {
        for t in OFFICE_TEMPLATES {
            let normalised = normalise_cron(t.cron);
            assert!(
                normalised.parse::<cron::Schedule>().is_ok(),
                "template '{}' has an unparseable cron '{}'",
                t.id,
                t.cron,
            );
        }
    }

    #[test]
    fn every_template_field_is_populated() {
        for t in OFFICE_TEMPLATES {
            assert!(!t.id.trim().is_empty(), "template id empty");
            assert!(!t.name.trim().is_empty(), "template '{}' name empty", t.id);
            assert!(!t.cron.trim().is_empty(), "template '{}' cron empty", t.id);
            assert!(
                !t.description.trim().is_empty(),
                "template '{}' description empty",
                t.id
            );
            assert!(
                !t.prompt.trim().is_empty(),
                "template '{}' prompt empty",
                t.id
            );
        }
    }

    #[test]
    fn template_ids_are_unique() {
        let ids: HashSet<&str> = OFFICE_TEMPLATES.iter().map(|t| t.id).collect();
        assert_eq!(
            ids.len(),
            OFFICE_TEMPLATES.len(),
            "duplicate template id detected",
        );
    }

    #[test]
    fn json_shape_round_trips() {
        let arr = templates_json();
        assert_eq!(arr.len(), OFFICE_TEMPLATES.len());
        for (v, t) in arr.iter().zip(OFFICE_TEMPLATES.iter()) {
            assert_eq!(v["id"].as_str(), Some(t.id));
            assert_eq!(v["cron"].as_str(), Some(t.cron));
            assert_eq!(v["prompt"].as_str(), Some(t.prompt));
        }
    }
}
