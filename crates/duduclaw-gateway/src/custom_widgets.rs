//! Custom dashboard widgets (2026-07-16 design:
//! `commercial/docs/custom-widgets-design-2026-07-16.md`).
//!
//! A custom widget is a **single-file HTML document** rendered by the
//! dashboard inside a sandboxed iframe (`sandbox="allow-scripts"`, no
//! `allow-same-origin`) with a read-only postMessage data bridge. Two
//! authoring paths share this store: hand-written HTML (admin / distributor
//! engineers, `origin = "html"`) and AI-generated from a guided
//! natural-language flow (any user, `origin = "ai"`).
//!
//! Security model lives in the RENDERER (unique-origin sandbox + injected CSP
//! + allowlisted bridge); the store's job is ownership, visibility and size
//! discipline. Widgets are referenced from per-user dashboard layouts as
//! `custom:<id>` entries; the layout stores only the id and the HTML is
//! lazy-loaded, so the 256 KB per-widget budget never bloats layout reads.

use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

/// Hard cap on a widget's HTML source. Large enough for a self-contained
/// dashboard card (inline CSS/JS, data: images), small enough that a runaway
/// generation or paste can't balloon the store.
pub const MAX_WIDGET_HTML_BYTES: usize = 256 * 1024;

/// Hard caps for the human-facing fields.
pub const MAX_TITLE_CHARS: usize = 80;
pub const MAX_DESCRIPTION_CHARS: usize = 500;

/// Layout-id prefix marking a custom widget in `dashboard.layout` entries.
pub const LAYOUT_ID_PREFIX: &str = "custom:";

/// Default per-user cap on custom widgets. Large enough for real dashboard
/// use (guided generation + hand-written + imports), small enough that a
/// runaway "generate a widget" loop or a scripted import can't unboundedly
/// grow one user's rows / list payload.
pub const MAX_WIDGETS_PER_USER_DEFAULT: usize = 20;

/// The effective per-user widget cap. Reads `DUDUCLAW_MAX_WIDGETS_PER_USER`
/// (a non-negative integer; `0` = unlimited) as an operator override, else
/// [`MAX_WIDGETS_PER_USER_DEFAULT`] — same override convention as
/// `EditionProfile::personal_max_agents()`.
pub fn max_widgets_per_user() -> usize {
    std::env::var("DUDUCLAW_MAX_WIDGETS_PER_USER")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(MAX_WIDGETS_PER_USER_DEFAULT)
}

/// How a widget was authored — drives which edit surface reopens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetOrigin {
    /// Hand-written HTML (admin-only authoring surface).
    Html,
    /// Generated from the guided natural-language flow.
    Ai,
}

impl WidgetOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Ai => "ai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "html" => Some(Self::Html),
            "ai" => Some(Self::Ai),
            _ => None,
        }
    }
}

/// One stored custom widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomWidget {
    pub id: String,
    pub title: String,
    pub description: String,
    pub html: String,
    pub origin: WidgetOrigin,
    pub created_by_user: String,
    /// `true` → visible to every authenticated user of this instance (the
    /// P3 "team sharing" page); `false` → owner-only.
    pub shared: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Validate the human-facing + HTML fields shared by create/update.
/// Fail-closed: any violation is an `Err` with a zh-TW operator message.
pub fn validate_widget_fields(title: &str, description: &str, html: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("標題不可為空".into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("標題最長 {MAX_TITLE_CHARS} 字"));
    }
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!("描述最長 {MAX_DESCRIPTION_CHARS} 字"));
    }
    if html.trim().is_empty() {
        return Err("widget HTML 不可為空".into());
    }
    if html.len() > MAX_WIDGET_HTML_BYTES {
        return Err(format!(
            "widget HTML 超過大小上限（{} KB）",
            MAX_WIDGET_HTML_BYTES / 1024
        ));
    }
    Ok(())
}

// ── AI generation (P2 — guided natural-language flow) ───────

/// Default model for widget generation. A one-off authoring action where
/// output quality dominates cost, so the main coding tier, not the utility
/// tier.
pub const GENERATE_MODEL: &str = "claude-sonnet-4-6";

/// Build the (system, user) prompt pair for widget generation.
///
/// `data_sources` / `style` come from the guided picker, `freeform` is the
/// user's own description. A revision round passes the prior HTML plus the
/// user's feedback.
pub fn build_generation_prompt(
    data_sources: &[String],
    style: &str,
    freeform: &str,
    prior_html: Option<&str>,
    feedback: Option<&str>,
) -> (String, String) {
    let system = r#"You generate a single self-contained HTML FRAGMENT for a dashboard widget card. It runs inside a sandboxed iframe with a strict CSP: no external scripts, styles, images, fonts, and no network access (fetch/XHR are blocked). All CSS and JS must be inline.

Data access is ONLY via the injected bridge `window.duduclaw.call(method)` (returns a Promise):
- 'agents.summary'  → { agents: [{ name, display_name, role, department, archived }] }
- 'tasks.summary'   → { total, by_status: { <status>: count }, completed_today, recent: [{ id, title, status, assignee, completed_at }] }
- 'cost.summary'    → cost totals object (inspect defensively; fields may vary)
- 'channels.status' → { channels: [{ channel, connected }] }
- 'system.status'   → { version, uptime_seconds, agents_count, channels_connected, edition_profile }
`window.duduclaw.onTheme(cb)` fires with 'light' | 'dark'.

Rules:
- Output ONLY the HTML fragment. No markdown fences, no explanations, no <!doctype>/<html>/<head>/<body> wrappers.
- Use the provided CSS variables for colors: --fg, --muted, --accent, --card, --border. Dark mode is `:root[data-theme="dark"]` — using the variables makes it automatic.
- All user-visible text in Traditional Chinese (zh-TW).
- Keep it compact (aim under 400px tall). Show a brief loading state, and a friendly error state if a bridge call fails.
- Never invent bridge methods beyond the five listed."#
        .to_string();

    let mut user = String::new();
    if !data_sources.is_empty() {
        user.push_str(&format!("資料來源：{}\n", data_sources.join("、")));
    }
    if !style.trim().is_empty() {
        user.push_str(&format!("呈現型態：{}\n", style.trim()));
    }
    if !freeform.trim().is_empty() {
        user.push_str(&format!("需求描述：{}\n", freeform.trim()));
    }
    if let (Some(prior), Some(fb)) = (prior_html, feedback) {
        user.push_str(&format!(
            "\n這是上一版產出的 widget HTML：\n{prior}\n\n使用者的修改回饋：{}\n請依回饋輸出修改後的完整 HTML fragment。",
            fb.trim()
        ));
    }
    if user.is_empty() {
        user.push_str("請產生一個顯示 AI 員工總數的簡單數字卡。");
    }
    // Recency reinforcement: the 2026-07-16 live test showed the model
    // narrating its work instead of emitting HTML when the output rule only
    // lived in the (distant) system prompt. Restate it as the LAST line.
    user.push_str(
        "\n\n重要：只輸出 HTML fragment 本身，第一個字元必須是 `<`。不要任何說明、前言、markdown 圍欄或總結文字。",
    );
    (system, user)
}

/// Strip a wrapping markdown code fence if the model added one despite
/// instructions (defensive; keeps the raw fragment otherwise untouched).
pub fn strip_html_fence(raw: &str) -> String {
    let t = raw.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t.to_string();
    };
    // Drop the info string (e.g. "html") up to the first newline.
    let body = rest.split_once('\n').map(|(_, b)| b).unwrap_or("");
    body.trim_end()
        .strip_suffix("```")
        .unwrap_or(body)
        .trim()
        .to_string()
}

/// Reduce a model response to the HTML fragment it (should) contain:
/// fence-strip, then cut any leading prose before the first `<` and any
/// trailing prose after the last `>` — the 2026-07-16 live test showed
/// models occasionally narrating around the markup despite instructions.
/// `Err` when no markup is present at all (pure prose ⇒ generation failed).
pub fn extract_html_fragment(raw: &str) -> Result<String, String> {
    let t = strip_html_fence(raw);
    let start = t.find('<').ok_or_else(|| "模型未輸出 HTML 內容".to_string())?;
    let end = t.rfind('>').ok_or_else(|| "模型未輸出 HTML 內容".to_string())?;
    if end < start {
        return Err("模型未輸出 HTML 內容".into());
    }
    Ok(t[start..=end].to_string())
}

/// SQLite-backed store at `<home>/custom_widgets.db`.
pub struct CustomWidgetStore {
    conn: Mutex<Connection>,
}

impl CustomWidgetStore {
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("custom_widgets.db");
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open custom widgets store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "CustomWidgetStore initialized");
        Ok(Self { conn: Mutex::new(conn) })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS custom_widgets (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                html TEXT NOT NULL,
                origin TEXT NOT NULL,
                created_by_user TEXT NOT NULL,
                shared INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_custom_widgets_owner
                ON custom_widgets(created_by_user);
            CREATE INDEX IF NOT EXISTS idx_custom_widgets_shared
                ON custom_widgets(shared);",
        )
        .map_err(|e| format!("init custom widgets schema: {e}"))
    }

    /// Create a widget; returns the new id. Enforces the per-user quota
    /// ([`max_widgets_per_user`]) here in the store — not just at the RPC
    /// layer — the same fail-closed philosophy as the ownership checks below.
    pub async fn create(
        &self,
        title: &str,
        description: &str,
        html: &str,
        origin: WidgetOrigin,
        created_by_user: &str,
    ) -> Result<String, String> {
        self.create_with_cap(title, description, html, origin, created_by_user, max_widgets_per_user())
            .await
    }

    /// Same as [`create`](Self::create) but takes the cap explicitly. Lets
    /// tests exercise quota enforcement deterministically without mutating
    /// the process-global `DUDUCLAW_MAX_WIDGETS_PER_USER` env var, which
    /// would race other tests running in the same process.
    async fn create_with_cap(
        &self,
        title: &str,
        description: &str,
        html: &str,
        origin: WidgetOrigin,
        created_by_user: &str,
        cap: usize,
    ) -> Result<String, String> {
        validate_widget_fields(title, description, html)?;
        if cap > 0 {
            let owned = self.count_owned(created_by_user).await?;
            if owned >= cap {
                return Err(format!(
                    "每人最多可建立 {cap} 個自訂 widget，請先刪除不用的再建立"
                ));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO custom_widgets
                (id, title, description, html, origin, created_by_user, shared, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
            params![id, title.trim(), description.trim(), html, origin.as_str(), created_by_user, now],
        )
        .map_err(|e| format!("create widget: {e}"))?;
        Ok(id)
    }

    /// Count widgets owned (created) by `user_id` — used for quota
    /// enforcement in [`create`](Self::create).
    pub async fn count_owned(&self, user_id: &str) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM custom_widgets WHERE created_by_user = ?1",
            params![user_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| c as usize)
        .map_err(|e| format!("count owned widgets: {e}"))
    }

    /// Fetch one widget by id (including HTML).
    pub async fn get(&self, id: &str) -> Result<Option<CustomWidget>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, title, description, html, origin, created_by_user, shared,
                    created_at, updated_at
             FROM custom_widgets WHERE id = ?1",
            params![id],
            row_to_widget,
        )
        .optional()
        .map_err(|e| format!("get widget: {e}"))
    }

    /// Every widget VISIBLE to `user_id`: their own plus anyone's shared.
    /// HTML column is loaded (rows are few and capped); RPC layers decide
    /// whether to strip it for list payloads.
    pub async fn list_visible(&self, user_id: &str) -> Result<Vec<CustomWidget>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, description, html, origin, created_by_user, shared,
                        created_at, updated_at
                 FROM custom_widgets
                 WHERE created_by_user = ?1 OR shared = 1
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| format!("prepare list: {e}"))?;
        let rows = stmt
            .query_map(params![user_id], row_to_widget)
            .map_err(|e| format!("query list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list: {e}"))?;
        Ok(rows)
    }

    /// Update title/description/html (any subset). Only the stored owner may
    /// update — enforced here, not just at the RPC layer (fail-closed).
    pub async fn update(
        &self,
        id: &str,
        actor_user: &str,
        title: Option<&str>,
        description: Option<&str>,
        html: Option<&str>,
    ) -> Result<(), String> {
        let existing = self.get(id).await?.ok_or_else(|| "找不到此 widget".to_string())?;
        if existing.created_by_user != actor_user {
            return Err("只有建立者可以編輯此 widget".into());
        }
        let new_title = title.unwrap_or(&existing.title);
        let new_desc = description.unwrap_or(&existing.description);
        let new_html = html.unwrap_or(&existing.html);
        validate_widget_fields(new_title, new_desc, new_html)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE custom_widgets
             SET title = ?1, description = ?2, html = ?3, updated_at = ?4
             WHERE id = ?5",
            params![new_title.trim(), new_desc.trim(), new_html, now, id],
        )
        .map_err(|e| format!("update widget: {e}"))?;
        Ok(())
    }

    /// Set the instance-wide sharing flag. Owner only.
    pub async fn set_shared(&self, id: &str, actor_user: &str, shared: bool) -> Result<(), String> {
        let existing = self.get(id).await?.ok_or_else(|| "找不到此 widget".to_string())?;
        if existing.created_by_user != actor_user {
            return Err("只有建立者可以變更分享狀態".into());
        }
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE custom_widgets SET shared = ?1, updated_at = ?2 WHERE id = ?3",
            params![shared as i64, now, id],
        )
        .map_err(|e| format!("share widget: {e}"))?;
        Ok(())
    }

    /// Delete a widget. Owner may delete their own; `actor_is_admin` may
    /// delete anyone's (the gallery moderation path).
    pub async fn remove(&self, id: &str, actor_user: &str, actor_is_admin: bool) -> Result<(), String> {
        let existing = self.get(id).await?.ok_or_else(|| "找不到此 widget".to_string())?;
        if existing.created_by_user != actor_user && !actor_is_admin {
            return Err("只有建立者或管理員可以刪除此 widget".into());
        }
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM custom_widgets WHERE id = ?1", params![id])
            .map_err(|e| format!("remove widget: {e}"))?;
        Ok(())
    }

    /// May `user_id` render this widget? (Own it, or it is shared.)
    pub async fn visible_to(&self, id: &str, user_id: &str) -> Result<bool, String> {
        Ok(self
            .get(id)
            .await?
            .map(|w| w.shared || w.created_by_user == user_id)
            .unwrap_or(false))
    }
}

fn row_to_widget(row: &rusqlite::Row<'_>) -> rusqlite::Result<CustomWidget> {
    let origin_raw: String = row.get(4)?;
    Ok(CustomWidget {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        html: row.get(3)?,
        origin: WidgetOrigin::parse(&origin_raw).unwrap_or(WidgetOrigin::Html),
        created_by_user: row.get(5)?,
        shared: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_get_roundtrip_and_visibility() {
        let store = CustomWidgetStore::open_in_memory().unwrap();
        let id = store
            .create("成本卡", "今日成本", "<div>hi</div>", WidgetOrigin::Html, "alice")
            .await
            .unwrap();
        let w = store.get(&id).await.unwrap().unwrap();
        assert_eq!(w.title, "成本卡");
        assert_eq!(w.origin, WidgetOrigin::Html);
        assert!(!w.shared);

        // Private → owner sees it, others don't.
        assert!(store.visible_to(&id, "alice").await.unwrap());
        assert!(!store.visible_to(&id, "bob").await.unwrap());
        assert_eq!(store.list_visible("bob").await.unwrap().len(), 0);

        // Shared → everyone sees it.
        store.set_shared(&id, "alice", true).await.unwrap();
        assert!(store.visible_to(&id, "bob").await.unwrap());
        assert_eq!(store.list_visible("bob").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ownership_enforced_in_store_not_just_rpc() {
        let store = CustomWidgetStore::open_in_memory().unwrap();
        let id = store
            .create("t", "", "<div/>", WidgetOrigin::Ai, "alice")
            .await
            .unwrap();
        assert!(store.update(&id, "mallory", Some("x"), None, None).await.is_err());
        assert!(store.set_shared(&id, "mallory", true).await.is_err());
        assert!(store.remove(&id, "mallory", false).await.is_err());
        // Admin may remove anyone's (moderation).
        store.remove(&id, "mallory", true).await.unwrap();
        assert!(store.get(&id).await.unwrap().is_none());
    }

    #[test]
    fn fence_stripping_is_defensive_only() {
        assert_eq!(strip_html_fence("<div>x</div>"), "<div>x</div>");
        assert_eq!(strip_html_fence("```html\n<div>x</div>\n```"), "<div>x</div>");
        assert_eq!(strip_html_fence("```\n<div>x</div>\n```"), "<div>x</div>");
    }

    #[test]
    fn fragment_extraction_cuts_surrounding_prose() {
        assert_eq!(extract_html_fragment("<div>x</div>").unwrap(), "<div>x</div>");
        // Narration around the markup (the observed live-test failure mode).
        assert_eq!(
            extract_html_fragment("輸出如下：\n<div>x</div>\n以上就是內容。").unwrap(),
            "<div>x</div>"
        );
        assert_eq!(
            extract_html_fragment("```html\n說明\n<div>x</div>\n```").unwrap(),
            "<div>x</div>"
        );
        // Pure prose → hard error, never stored.
        assert!(extract_html_fragment("我無法產生這個 widget").is_err());
    }

    #[test]
    fn generation_prompt_carries_revision_context() {
        let (_, user) = build_generation_prompt(
            &["tasks".into()],
            "數字卡",
            "今日完成數",
            Some("<div>v1</div>"),
            Some("字太小"),
        );
        assert!(user.contains("<div>v1</div>"));
        assert!(user.contains("字太小"));
        let (_, blank) = build_generation_prompt(&[], "", "", None, None);
        assert!(!blank.is_empty());
    }

    #[test]
    fn widget_quota_default_is_20() {
        assert_eq!(MAX_WIDGETS_PER_USER_DEFAULT, 20);
    }

    #[tokio::test]
    async fn count_owned_reflects_created_widgets() {
        let store = CustomWidgetStore::open_in_memory().unwrap();
        assert_eq!(store.count_owned("alice").await.unwrap(), 0);
        store.create("a", "", "<div/>", WidgetOrigin::Html, "alice").await.unwrap();
        store.create("b", "", "<div/>", WidgetOrigin::Html, "alice").await.unwrap();
        store.create("c", "", "<div/>", WidgetOrigin::Html, "bob").await.unwrap();
        assert_eq!(store.count_owned("alice").await.unwrap(), 2);
        assert_eq!(store.count_owned("bob").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn create_rejects_once_cap_reached() {
        // Uses `create_with_cap` (explicit cap) rather than mutating env vars,
        // which would race other tests running in the same process.
        let store = CustomWidgetStore::open_in_memory().unwrap();
        for i in 0..2 {
            store
                .create_with_cap(&format!("w{i}"), "", "<div/>", WidgetOrigin::Html, "alice", 2)
                .await
                .unwrap();
        }
        let err = store
            .create_with_cap("w3", "", "<div/>", WidgetOrigin::Html, "alice", 2)
            .await
            .unwrap_err();
        assert!(err.contains('2'), "error should mention the cap: {err}");

        // A different owner is unaffected by alice's quota.
        store
            .create_with_cap("other", "", "<div/>", WidgetOrigin::Html, "bob", 2)
            .await
            .unwrap();

        // cap = 0 means unlimited, even for a user already at/over a nonzero cap.
        for i in 0..5 {
            store
                .create_with_cap(&format!("u{i}"), "", "<div/>", WidgetOrigin::Html, "alice", 0)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn validation_rejects_oversize_and_empty() {
        let store = CustomWidgetStore::open_in_memory().unwrap();
        assert!(store
            .create("", "", "<div/>", WidgetOrigin::Html, "a")
            .await
            .is_err());
        assert!(store
            .create("t", "", "", WidgetOrigin::Html, "a")
            .await
            .is_err());
        let big = "x".repeat(MAX_WIDGET_HTML_BYTES + 1);
        assert!(store.create("t", "", &big, WidgetOrigin::Html, "a").await.is_err());
    }
}
