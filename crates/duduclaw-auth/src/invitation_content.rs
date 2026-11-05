//! Localized invitation copy. This store never contains invitation credentials.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use crate::{UserDb, UserRole, UserStatus};

pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS invitation_content (
        locale TEXT PRIMARY KEY, content TEXT NOT NULL
    );").map_err(|_| "invitation_store_unavailable".to_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationContent {
    pub opening: String,
    pub steps: String,
    pub examples: [String; 3],
    pub course_url: String,
}

pub fn valid_locale(locale: &str) -> bool { matches!(locale, "zh-TW" | "en" | "ja-JP") }

impl InvitationContent {
    pub fn defaults(locale: &str) -> Self {
        let (opening, steps, examples) = match locale {
            "en" => (
                "Welcome! Your AI employee can help with your everyday work.",
                "1. Set your password using the invitation link.\n2. Sign in with your email and new password.\n3. Open a new chat and try one of the examples below.",
                ["Summarize the attached document in five key points and list anything I should verify.", "Draft a polite reply to the message below. Let me review it before sending.", "Remind me tomorrow at 9 a.m. to review this week's progress. Confirm the date and time zone first."],
            ),
            "ja-JP" => (
                "ようこそ！AI スタッフが日々の業務をお手伝いします。",
                "1. 招待リンクからパスワードを設定します。\n2. メールアドレスと新しいパスワードでログインします。\n3. 新しい会話を開き、下の例を試してみましょう。",
                ["添付資料を5つの要点にまとめ、確認が必要な箇所を挙げてください。", "以下のメッセージへの丁寧な返信案を作ってください。送信前に確認させてください。", "明日の午前9時に今週の進捗を確認するようリマインドしてください。先に日付とタイムゾーンを確認してください。"],
            ),
            _ => (
                "歡迎加入！你的 AI 員工可以協助處理日常工作。",
                "1. 點選邀請連結，設定自己的密碼。\n2. 使用電子郵件與新密碼登入。\n3. 開啟新對話，試試下方的上手範例。",
                ["請把附件整理成五個重點，並列出需要我再確認的地方。", "請根據下面這則訊息，擬一封有禮貌的回覆，先讓我確認再寄出。", "請在明天上午九點提醒我整理本週進度，先跟我確認日期與時區。"],
            ),
        };
        Self { opening: opening.into(), steps: steps.into(), examples: examples.map(str::to_owned), course_url: String::new() }
    }

    pub fn validate(&self) -> Result<(), String> {
        for (text, max) in [(&self.opening, 4000), (&self.steps, 8000)] {
            if text.trim().is_empty() || text.chars().count() > max || text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
                return Err("invalid_invitation_content".into());
            }
        }
        if self.examples.iter().any(|s| s.trim().is_empty() || s.chars().count() > 1000 || s.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
            || self.course_url.len() > 2048 {
            return Err("invalid_invitation_content".into());
        }
        Ok(())
    }
}

impl UserDb {
    pub fn invitation_content(&self, locale: &str) -> Result<InvitationContent, String> {
        if !valid_locale(locale) { return Err("invalid_locale".into()); }
        let raw: Option<String> = self.conn().query_row("SELECT content FROM invitation_content WHERE locale=?1", [locale], |r| r.get(0))
            .optional().map_err(|_| "invitation_store_unavailable".to_owned())?;
        let content = match raw {
            Some(raw) => serde_json::from_str(&raw).map_err(|_| "invitation_store_unavailable".to_owned())?,
            None => InvitationContent::defaults(locale),
        };
        content.validate()?;
        Ok(content)
    }

    pub fn update_invitation_content(&self, actor: &str, epoch: u64, locale: &str, content: &InvitationContent) -> Result<(), String> {
        if !valid_locale(locale) { return Err("invalid_locale".into()); }
        content.validate()?;
        let encoded = serde_json::to_string(content).map_err(|_| "invalid_invitation_content".to_owned())?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| "invitation_store_unavailable".to_owned())?;
        let user = crate::user_row::load_user(&tx, actor)?.ok_or("permission_denied")?;
        if user.role != UserRole::Admin || user.status != UserStatus::Active || user.must_change_password || user.credential_epoch != epoch {
            return Err("permission_denied".into());
        }
        tx.execute("INSERT INTO invitation_content(locale,content) VALUES(?1,?2) ON CONFLICT(locale) DO UPDATE SET content=excluded.content", params![locale, encoded])
            .map_err(|_| "invitation_store_unavailable".to_owned())?;
        tx.execute("INSERT INTO auth_audit_log(user_id,action,target,timestamp) VALUES(?1,'invitation_content.updated',?2,?3)", params![actor, locale, chrono::Utc::now().to_rfc3339()])
            .map_err(|_| "invitation_store_unavailable".to_owned())?;
        tx.commit().map_err(|_| "invitation_store_unavailable".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invitation_content_defaults_and_live_admin_gate() {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@example.test", "Admin", "password123", UserRole::Admin).unwrap();
        let employee = db.create_user("user@example.test", "User", "password123", UserRole::Employee).unwrap();
        for locale in ["zh-TW", "en", "ja-JP"] {
            let mut content = db.invitation_content(locale).unwrap();
            content.validate().unwrap();
            content.opening = "Custom welcome".into();
            assert!(db.update_invitation_content(&employee.id, employee.credential_epoch, locale, &content).is_err());
            assert!(db.update_invitation_content(&admin.id, admin.credential_epoch + 1, locale, &content).is_err());
            db.update_invitation_content(&admin.id, admin.credential_epoch, locale, &content).unwrap();
            assert_eq!(db.invitation_content(locale).unwrap().opening, "Custom welcome");
        }
        assert!(db.invitation_content("unknown").is_err());
        let mut bad = InvitationContent::defaults("en");
        bad.examples[1] = String::new();
        assert!(db.update_invitation_content(&admin.id, admin.credential_epoch, "en", &bad).is_err());
        assert_eq!(db.query_audit_log(None, Some("invitation_content.updated"), 20).unwrap().len(), 3);
    }
}
