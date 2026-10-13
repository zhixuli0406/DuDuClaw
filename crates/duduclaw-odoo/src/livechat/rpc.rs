//! JSON-RPC calls the Live Chat bridge makes, as the bot user.
//!
//! Every model, field and method here was exercised against a live Odoo 18.0
//! (2026-10-12): `bus.presence.update_presence` keeps the bot `online`,
//! `discuss.channel` with `channel_type = 'livechat'` is one visitor session,
//! visitor messages carry `author_guest_id` (anonymous) or `author_id`
//! (logged-in), `message_post(body_is_html=True)` is honoured for internal
//! users, and `add_members(partner_ids=…)` invites a human operator. The same
//! calls were re-run on Odoo 19.0 (field differences: see [`Dialect`]).
//! Odoo 16 and older (`mail.channel`) are not supported.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use crate::connector::OdooConnector;

/// Sessions read per poll. A bot serving more concurrent chats than this is
/// far outside what one operator account is for.
const SESSION_LIMIT: usize = 200;
/// Messages read per poll; the cursor advances, so a backlog drains over
/// several polls.
const MESSAGE_LIMIT: usize = 500;

/// One live chat session (`discuss.channel`).
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: i64,
    pub livechat_channel_id: Option<i64>,
    pub country_id: Option<i64>,
    pub anonymous_name: String,
    pub chatbot_step_id: Option<i64>,
    /// `channel_partner_ids`: everyone in the session, bot included.
    pub member_partner_ids: Vec<i64>,
}

/// One `comment` message in a session.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub id: i64,
    pub session_id: i64,
    pub author_partner_id: Option<i64>,
    pub author_guest_id: Option<i64>,
    pub body_html: String,
    /// Odoo UTC timestamp, `YYYY-MM-DD HH:MM:SS`.
    pub create_date: String,
}

/// A many2one value is `[id, "name"]` or `false`.
pub fn m2o_id(v: &Value) -> Option<i64> {
    v.as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_i64)
        .or_else(|| v.as_i64())
        .filter(|id| *id > 0)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn records(v: Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Array(a) => Ok(a),
        other => Err(format!("Unexpected Odoo response: {other}")),
    }
}

fn uid(conn: &OdooConnector) -> Result<i64, String> {
    conn.uid.ok_or_else(|| "Not authenticated".to_string())
}

/// Odoo answers a call on a model it does not have with "Object … doesn't
/// exist"; turn that into an actionable message.
pub fn explain_missing_model(err: &str) -> String {
    if err.contains("discuss.channel") && err.contains("exist") {
        "這個 Odoo 沒有 discuss.channel：需要 Odoo 17 以上並安裝 Live Chat（im_livechat）模組".to_string()
    } else {
        err.to_string()
    }
}

/// Field and API differences between the Odoo versions the bridge supports,
/// detected once per connection (never guessed from the version string:
/// `saas~` releases change fields between majors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dialect {
    /// 17/18: `livechat_active` (bool). 19+: `livechat_end_dt` (empty = open).
    pub has_livechat_active: bool,
    /// Removed in 19 (visitors are `mail.guest` records there).
    pub has_anonymous_name: bool,
}

/// Probe `discuss.channel` fields to pick the session dialect.
pub async fn detect_dialect(conn: &OdooConnector) -> Result<Dialect, String> {
    let fields = conn
        .execute_kw(
            "discuss.channel",
            "fields_get",
            vec![json!(["livechat_active", "livechat_end_dt", "anonymous_name", "livechat_operator_id"])],
            json!({"attributes": ["type"]}),
        )
        .await
        .map_err(|e| explain_missing_model(&e))?;
    let has = |f: &str| fields.get(f).is_some();
    if !has("livechat_operator_id") {
        return Err("這個 Odoo 沒有安裝 Live Chat（im_livechat）模組".to_string());
    }
    if !has("livechat_active") && !has("livechat_end_dt") {
        return Err("無法辨識這個 Odoo 版本的 Live Chat 欄位".to_string());
    }
    Ok(Dialect {
        has_livechat_active: has("livechat_active"),
        has_anonymous_name: has("anonymous_name"),
    })
}

/// How the bot's `online` presence is kept up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceApi {
    /// Odoo 17/18: the bot calls the public `bus.presence.update_presence`.
    Bus,
    /// Odoo 19+: `mail.presence` is writable by administrators only, so an
    /// admin connection writes the bot user's row.
    Mail,
}

/// Whether an error means "this Odoo has no `bus.presence`" (Odoo 19+).
pub fn is_missing_bus_presence(err: &str) -> bool {
    err.contains("bus.presence") && err.contains("exist")
}

/// Odoo 17/18: keep the calling bot user `online` so Odoo assigns sessions
/// to it. Odoo marks a user offline only when one of its websockets closes or
/// after 12 h without a poll, so a heartbeat every ~30 s is plenty.
pub async fn heartbeat_bus(conn: &OdooConnector) -> Result<(), String> {
    let uid = uid(conn)?;
    conn.execute_kw(
        "bus.presence",
        "update_presence",
        vec![json!(0), json!("user_id"), json!(uid)],
        json!({}),
    )
    .await
    .map(|_| ())
}

fn odoo_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Odoo 19+: write `bot_uid`'s `mail.presence` row through `admin` (a user in
/// `base.group_system`). Creates the row on first use.
pub async fn heartbeat_mail(admin: &OdooConnector, bot_uid: i64) -> Result<(), String> {
    let now = odoo_now();
    let ids = presence_ids(admin, "mail.presence", bot_uid).await?;
    let values = json!({"status": "online", "last_poll": now, "last_presence": now});
    if ids.is_empty() {
        let mut create = values;
        create["user_id"] = json!(bot_uid);
        admin.create("mail.presence", create).await.map(|_| ())
    } else {
        admin.write("mail.presence", &ids, values).await.map(|_| ())
    }
    .map_err(|e| {
        if e.contains("not allowed") {
            "Odoo 19 只允許管理員寫入上線狀態：請讓 [odoo] 連線帳號具備「管理員」角色".to_string()
        } else {
            e
        }
    })
}

async fn presence_ids(conn: &OdooConnector, model: &str, user_id: i64) -> Result<Vec<i64>, String> {
    let ids = conn
        .execute_kw(model, "search", vec![json!([["user_id", "=", user_id]])], json!({}))
        .await?;
    Ok(ids
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default())
}

/// Mark the bot offline so Odoo stops assigning new sessions to it (bridge
/// disabled or gateway shutting down). `conn` is the bot connection for
/// [`PresenceApi::Bus`] and the admin connection for [`PresenceApi::Mail`].
pub async fn set_offline(conn: &OdooConnector, api: PresenceApi, bot_uid: i64) -> Result<(), String> {
    let model = match api {
        PresenceApi::Bus => "bus.presence",
        PresenceApi::Mail => "mail.presence",
    };
    let ids = presence_ids(conn, model, bot_uid).await?;
    if ids.is_empty() {
        return Ok(());
    }
    conn.write(model, &ids, json!({"status": "offline"}))
        .await
        .map(|_| ())
}

/// The bot user's own partner id and current IM status.
pub async fn bot_identity(conn: &OdooConnector) -> Result<(i64, String, String), String> {
    let uid = uid(conn)?;
    let rows = records(
        conn.execute_kw(
            "res.users",
            "read",
            vec![json!([uid])],
            json!({"fields": ["partner_id", "name", "im_status"]}),
        )
        .await?,
    )?;
    let row = rows.first().ok_or("Bot user not readable")?;
    let partner = m2o_id(row.get("partner_id").unwrap_or(&Value::Null))
        .ok_or("Bot user has no partner")?;
    Ok((partner, str_field(row, "name"), str_field(row, "im_status")))
}

/// Ongoing sessions the bot is a member of. Membership, not
/// `livechat_operator_id`: when a Chatbot script forwards a visitor, Odoo
/// adds the bot as a member and leaves the script persona as the operator.
pub async fn active_sessions(
    conn: &OdooConnector,
    dialect: Dialect,
    bot_partner_id: i64,
) -> Result<Vec<Session>, String> {
    let open = if dialect.has_livechat_active {
        json!(["livechat_active", "=", true])
    } else {
        json!(["livechat_end_dt", "=", false])
    };
    let mut fields = vec![
        "livechat_channel_id",
        "country_id",
        "chatbot_current_step_id",
        "channel_partner_ids",
    ];
    if dialect.has_anonymous_name {
        fields.push("anonymous_name");
    }
    let rows = records(
        conn.execute_kw(
            "discuss.channel",
            "search_read",
            vec![json!([
                ["channel_type", "=", "livechat"],
                open,
                ["channel_partner_ids", "in", [bot_partner_id]],
            ])],
            json!({
                "fields": fields,
                "limit": SESSION_LIMIT,
                "order": "id asc",
            }),
        )
        .await
        .map_err(|e| explain_missing_model(&e))?,
    )?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(Session {
                id: r.get("id")?.as_i64()?,
                livechat_channel_id: m2o_id(r.get("livechat_channel_id")?),
                country_id: r.get("country_id").and_then(m2o_id),
                anonymous_name: str_field(r, "anonymous_name"),
                chatbot_step_id: r.get("chatbot_current_step_id").and_then(m2o_id),
                member_partner_ids: r
                    .get("channel_partner_ids")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// Odoo Polish-notation domain: messages of each session newer than that
/// session's own cursor. One cursor per session matters: a single shared
/// minimum would let one session (a new one at 0, or a flooded one) fill
/// every page with old messages and starve the rest.
pub fn cursor_domain(cursors: &[(i64, i64)]) -> Value {
    let mut domain = vec![
        json!(["model", "=", "discuss.channel"]),
        json!(["message_type", "=", "comment"]),
    ];
    for _ in 1..cursors.len() {
        domain.push(json!("|"));
    }
    for (session, after) in cursors {
        domain.push(json!("&"));
        domain.push(json!(["res_id", "=", session]));
        domain.push(json!(["id", ">", after]));
    }
    Value::Array(domain)
}

/// `comment` messages newer than each session's cursor, oldest first.
pub async fn messages_since(
    conn: &OdooConnector,
    cursors: &[(i64, i64)],
) -> Result<Vec<ChatMessage>, String> {
    if cursors.is_empty() {
        return Ok(Vec::new());
    }
    let rows = records(
        conn.execute_kw(
            "mail.message",
            "search_read",
            vec![cursor_domain(cursors)],
            json!({
                "fields": ["res_id", "author_id", "author_guest_id", "body", "create_date"],
                "limit": MESSAGE_LIMIT,
                "order": "id asc",
            }),
        )
        .await?,
    )?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(ChatMessage {
                id: r.get("id")?.as_i64()?,
                session_id: r.get("res_id")?.as_i64()?,
                author_partner_id: r.get("author_id").and_then(m2o_id),
                author_guest_id: r.get("author_guest_id").and_then(m2o_id),
                body_html: str_field(r, "body"),
                create_date: str_field(r, "create_date"),
            })
        })
        .collect())
}

/// Which of `partner_ids` belong to internal (non-portal) users — i.e. human
/// operators, as opposed to visitors and chatbot personas.
pub async fn internal_partner_ids(
    conn: &OdooConnector,
    partner_ids: &[i64],
) -> Result<HashSet<i64>, String> {
    if partner_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = records(
        conn.execute_kw(
            "res.users",
            "search_read",
            vec![json!([["partner_id", "in", partner_ids], ["share", "=", false]])],
            json!({"fields": ["partner_id"], "context": {"active_test": false}}),
        )
        .await?,
    )?;
    Ok(rows
        .iter()
        .filter_map(|r| r.get("partner_id").and_then(m2o_id))
        .collect())
}

/// Which of `partner_ids` are archived. Odoo creates every Chatbot script
/// persona (`chatbot.script.operator_partner_id`) as an archived partner
/// (17, 18 and 19 alike), while visitors and customers are active — and this
/// needs no access to `chatbot.script`, which a plain Live Chat user lacks on
/// Odoo 19.
pub async fn archived_partner_ids(
    conn: &OdooConnector,
    partner_ids: &[i64],
) -> Result<HashSet<i64>, String> {
    if partner_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let ids = conn
        .execute_kw(
            "res.partner",
            "search",
            vec![json!([["id", "in", partner_ids], ["active", "=", false]])],
            json!({"context": {"active_test": false}}),
        )
        .await?;
    Ok(ids
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default())
}

/// Post the bot's reply. `html` must already be escaped (see
/// [`super::reply_to_html`]). Odoo versions without `body_is_html` get the
/// plain text instead, which Odoo escapes itself.
pub async fn post_reply(
    conn: &OdooConnector,
    session_id: i64,
    html: &str,
    plain: &str,
) -> Result<i64, String> {
    let post = |body: &str, as_html: bool| {
        let mut kwargs = json!({
            "body": body,
            "message_type": "comment",
            "subtype_xmlid": "mail.mt_comment",
        });
        if as_html {
            kwargs["body_is_html"] = json!(true);
        }
        conn.execute_kw("discuss.channel", "message_post", vec![json!([session_id])], kwargs)
    };
    let result = match post(html, true).await {
        Ok(v) => v,
        Err(e) if e.contains("body_is_html") => post(plain, false).await?,
        Err(e) => return Err(e),
    };
    m2o_id(&result)
        .or_else(|| result.as_i64())
        .ok_or_else(|| format!("Unexpected message_post response: {result}"))
}

/// Invite human operators (by `res.users` id) into a session.
pub async fn invite_users(
    conn: &OdooConnector,
    session_id: i64,
    user_ids: &[i64],
) -> Result<usize, String> {
    if user_ids.is_empty() {
        return Ok(0);
    }
    let rows = records(
        conn.execute_kw(
            "res.users",
            "read",
            vec![json!(user_ids)],
            json!({"fields": ["partner_id"]}),
        )
        .await?,
    )?;
    let partners: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.get("partner_id").and_then(m2o_id))
        .collect();
    if partners.is_empty() {
        return Ok(0);
    }
    conn.execute_kw(
        "discuss.channel",
        "add_members",
        vec![json!([session_id])],
        json!({"partner_ids": partners}),
    )
    .await?;
    Ok(partners.len())
}

/// Visitor facts read once per session for audience routing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VisitorFacts {
    pub country_code: Option<String>,
    pub lang: Option<String>,
    pub logged_in: bool,
    pub partner_tags: Vec<String>,
    pub chatbot_script_id: Option<i64>,
}

/// Read the facts audience rules can use. A failed sub-read leaves that fact
/// unknown (so rules needing it simply do not match) instead of failing the
/// whole session — a visitor must never go unanswered over a tag lookup.
pub async fn visitor_facts(
    conn: &OdooConnector,
    session: &Session,
    author_partner_id: Option<i64>,
    author_guest_id: Option<i64>,
    country_codes: &mut HashMap<i64, String>,
) -> VisitorFacts {
    let mut facts = VisitorFacts::default();
    let mut country_id = session.country_id;

    if let Some(partner) = author_partner_id {
        facts.logged_in = true;
        if let Some(row) = read_one(conn, "res.partner", partner, &["lang", "category_id", "country_id"]).await {
            facts.lang = Some(str_field(&row, "lang")).filter(|s| !s.is_empty());
            country_id = country_id.or_else(|| row.get("country_id").and_then(m2o_id));
            facts.partner_tags = tag_names(conn, &row).await;
        }
    } else if let Some(guest) = author_guest_id
        && let Some(row) = read_one(conn, "mail.guest", guest, &["lang", "country_id"]).await
    {
        facts.lang = Some(str_field(&row, "lang")).filter(|s| !s.is_empty());
        country_id = country_id.or_else(|| row.get("country_id").and_then(m2o_id));
    }

    if let Some(cid) = country_id {
        facts.country_code = match country_codes.get(&cid) {
            Some(code) => Some(code.clone()),
            None => {
                let code = read_one(conn, "res.country", cid, &["code"])
                    .await
                    .map(|r| str_field(&r, "code"))
                    .filter(|c| !c.is_empty());
                if let Some(c) = &code {
                    country_codes.insert(cid, c.clone());
                }
                code
            }
        };
    }

    if let Some(step) = session.chatbot_step_id {
        facts.chatbot_script_id = read_one(conn, "chatbot.script.step", step, &["chatbot_script_id"])
            .await
            .and_then(|r| r.get("chatbot_script_id").and_then(m2o_id));
    }
    facts
}

/// `read` one record; any error or a missing row is `None`.
async fn read_one(conn: &OdooConnector, model: &str, id: i64, fields: &[&str]) -> Option<Value> {
    conn.execute_kw(model, "read", vec![json!([id])], json!({"fields": fields}))
        .await
        .and_then(records)
        .ok()?
        .into_iter()
        .next()
}

/// Names of a partner row's `category_id` tags (empty on any error).
async fn tag_names(conn: &OdooConnector, partner_row: &Value) -> Vec<String> {
    let tag_ids: Vec<i64> = partner_row
        .get("category_id")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if tag_ids.is_empty() {
        return Vec::new();
    }
    conn.execute_kw(
        "res.partner.category",
        "read",
        vec![json!(tag_ids)],
        json!({"fields": ["name"]}),
    )
    .await
    .and_then(records)
    .map(|tags| {
        tags.iter()
            .map(|t| str_field(t, "name"))
            .filter(|n| !n.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

// ── Dashboard helpers ───────────────────────────────────────────

/// `{id, name, operator_ids}` for every `im_livechat.channel`.
pub async fn list_livechat_channels(conn: &OdooConnector) -> Result<Vec<Value>, String> {
    let rows = records(
        conn.execute_kw(
            "im_livechat.channel",
            "search_read",
            vec![json!([])],
            json!({"fields": ["name", "user_ids"], "limit": 100}),
        )
        .await
        .map_err(|e| {
            if e.contains("im_livechat.channel") {
                "這個 Odoo 沒有安裝 Live Chat（im_livechat）模組".to_string()
            } else {
                e
            }
        })?,
    )?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get("id"),
                "name": str_field(r, "name"),
                "operator_ids": r.get("user_ids").cloned().unwrap_or(json!([])),
            })
        })
        .collect())
}

/// `{id, name}` for every Chatbot script (inactive ones included).
pub async fn list_chatbot_scripts(conn: &OdooConnector) -> Result<Vec<Value>, String> {
    let rows = records(
        conn.execute_kw(
            "chatbot.script",
            "search_read",
            vec![json!([])],
            json!({"fields": ["title"], "limit": 100, "context": {"active_test": false}}),
        )
        .await?,
    )?;
    Ok(rows
        .iter()
        .map(|r| json!({"id": r.get("id"), "name": str_field(r, "title")}))
        .collect())
}

/// `{id, name, login}` for internal users (handoff picker).
pub async fn list_internal_users(conn: &OdooConnector) -> Result<Vec<Value>, String> {
    let rows = records(
        conn.execute_kw(
            "res.users",
            "search_read",
            vec![json!([["share", "=", false]])],
            json!({"fields": ["name", "login"], "limit": 200, "order": "name asc"}),
        )
        .await?,
    )?;
    Ok(rows
        .iter()
        .map(|r| json!({"id": r.get("id"), "name": str_field(r, "name"), "login": str_field(r, "login")}))
        .collect())
}
