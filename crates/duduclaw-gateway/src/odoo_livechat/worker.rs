//! The poll loop: connect as the bot, keep it online, find new visitor
//! messages and hand each session with work to [`super::turn`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use duduclaw_odoo::livechat::rpc::{self, ChatMessage, Dialect, PresenceApi, Session};
use duduclaw_odoo::livechat::LivechatConfig;
use duduclaw_odoo::{OdooConfig, OdooConnector};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::turn::{self, TurnInput};
use super::{classify, update_status, with_state, Author, PresenceHandle, SessionState};
use crate::channel_reply::ReplyContext;

/// How often the bot's presence is refreshed. Odoo only drops a user to
/// offline on a closed websocket or after 12 h, so this is generous.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);
/// Idle cadence while the bridge is off or unconfigured.
const IDLE_TICK: Duration = Duration::from_secs(5);
/// Back-off after a connection or RPC failure.
const ERROR_BACKOFF: Duration = Duration::from_secs(30);
/// On first sight of a session (e.g. after a restart), visitor messages
/// older than this are not answered: a reply an hour late is a wrong reply.
const STALE_AFTER_MINUTES: i64 = 30;
/// Partner classification (operator / chatbot persona / visitor) is cached;
/// refresh it so a contact promoted to an internal user is seen as one.
const PARTNER_CACHE_TTL: Duration = Duration::from_secs(600);
/// Concurrent AI turns across all sessions.
const MAX_CONCURRENT_TURNS: usize = 4;

/// What a message author's partner is, besides the bot itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartnerKind {
    /// Belongs to an internal user: a human operator.
    Internal,
    /// Archived partner: a Chatbot script persona.
    ChatbotPersona,
    /// A visitor or customer.
    Other,
}

/// Live connection to Odoo for one configuration fingerprint.
struct Conns {
    fingerprint: String,
    /// [`super::instance_tag`] of the connected Odoo.
    instance: String,
    bot: Arc<OdooConnector>,
    /// Admin connection (the `[odoo]` user) for Odoo 19 presence writes.
    admin: Option<Arc<OdooConnector>>,
    dialect: Dialect,
    bot_uid: i64,
    bot_partner_id: i64,
    presence: Option<PresenceApi>,
    last_heartbeat: Option<Instant>,
}

/// Start the bridge loop. Always spawned; it idles while
/// `[odoo_livechat] enabled` is false so the dashboard can switch it on
/// without a gateway restart.
pub fn spawn(home: &Path, ctx: Arc<ReplyContext>) -> JoinHandle<()> {
    let home = home.to_path_buf();
    tokio::spawn(async move { run(home, ctx).await })
}

async fn run(home: PathBuf, ctx: Arc<ReplyContext>) {
    let mut conns: Option<Conns> = None;
    let mut partner_cache: HashMap<i64, PartnerKind> = HashMap::new();
    let mut partner_cache_at = Instant::now();
    let inflight: Arc<std::sync::Mutex<HashSet<i64>>> = Arc::default();
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_TURNS));

    loop {
        let (odoo_cfg, lc, table) = load_config(&home);
        let active = lc.enabled
            && odoo_cfg.is_configured()
            && duduclaw_core::gateway_instance::held(&home);
        if !active {
            if let Some(c) = conns.take() {
                go_offline(&c).await;
                info!("odoo livechat: bridge switched off — bot marked offline");
            }
            update_status(|s| {
                s.state = "off".into();
                s.message = if !lc.enabled {
                    String::new()
                } else if !odoo_cfg.is_configured() {
                    "尚未設定 Odoo 連線".into()
                } else {
                    "此程序不是主要閘道，不處理線上客服".into()
                };
            });
            tokio::time::sleep(IDLE_TICK).await;
            continue;
        }

        let fingerprint = fingerprint(&table);
        if conns.as_ref().is_some_and(|c| c.fingerprint != fingerprint) {
            if let Some(old) = conns.take() {
                go_offline(&old).await;
            }
            partner_cache.clear();
        }
        if conns.is_none() {
            update_status(|s| {
                s.state = "connecting".into();
                s.message.clear();
            });
            match connect(&home, &odoo_cfg, &lc, &table, fingerprint).await {
                Ok(c) => {
                    update_status(|s| {
                        s.bot_name = c.bot_name.clone();
                        s.bot_partner_id = Some(c.conns.bot_partner_id);
                    });
                    conns = Some(c.conns);
                }
                Err(e) => {
                    fail(&e);
                    tokio::time::sleep(ERROR_BACKOFF).await;
                    continue;
                }
            }
        }
        let Some(c) = conns.as_mut() else { continue };

        if let Err(e) = heartbeat(c).await {
            fail(&e);
            conns = None;
            tokio::time::sleep(ERROR_BACKOFF).await;
            continue;
        }

        if partner_cache_at.elapsed() > PARTNER_CACHE_TTL {
            partner_cache.clear();
            partner_cache_at = Instant::now();
        }
        let tick = poll_once(
            &home,
            &ctx,
            c,
            &lc,
            &mut partner_cache,
            &inflight,
            &permits,
        )
        .await;
        match tick {
            Ok(()) => update_status(|s| {
                s.state = "online".into();
                s.message.clear();
                s.last_poll_at = Some(chrono::Utc::now().to_rfc3339());
            }),
            Err(e) => {
                fail(&e);
                conns = None;
                tokio::time::sleep(ERROR_BACKOFF).await;
                continue;
            }
        }
        tokio::time::sleep(Duration::from_secs(lc.effective_poll_interval())).await;
    }
}

fn fail(e: &str) {
    warn!(error = %e, "odoo livechat: bridge error");
    update_status(|s| {
        s.state = "error".into();
        s.message = e.to_string();
        s.last_error_at = Some(chrono::Utc::now().to_rfc3339());
    });
}

fn load_config(home: &Path) -> (OdooConfig, LivechatConfig, toml::Table) {
    let table: toml::Table = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_default();
    (
        OdooConfig::from_toml(&table),
        LivechatConfig::from_toml(&table),
        table,
    )
}

/// Changes to anything that affects the connection force a reconnect.
/// Ciphertexts are compared as-is; nothing is decrypted here.
fn fingerprint(table: &toml::Table) -> String {
    let get = |section: &str, key: &str| {
        table
            .get(section)
            .and_then(|s| s.get(key))
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    [
        get("odoo", "url"),
        get("odoo", "db"),
        get("odoo", "username"),
        get("odoo", "auth_method"),
        get("odoo", "api_key_enc"),
        get("odoo", "password_enc"),
        get(duduclaw_odoo::livechat::SECTION, "bot_username"),
        get(duduclaw_odoo::livechat::SECTION, "bot_api_key_enc"),
    ]
    .join("\u{1f}")
}

struct Connected {
    conns: Conns,
    bot_name: String,
}

async fn connect(
    home: &Path,
    odoo_cfg: &OdooConfig,
    lc: &LivechatConfig,
    table: &toml::Table,
    fingerprint: String,
) -> Result<Connected, String> {
    let main_credential = crate::odoo_events::resolve_credential(home).await;
    let dedicated = !lc.bot_username.trim().is_empty();
    let bot = if dedicated {
        let credential = crate::config_crypto::decrypt_config_field_async(
            table,
            duduclaw_odoo::livechat::SECTION,
            "bot_api_key",
            home,
        )
        .await
        .map(|s| s.expose().to_string())
        .filter(|s| !s.is_empty())
        .ok_or("機器人帳號的 API 金鑰尚未設定或無法解密")?;
        let cfg = OdooConfig {
            username: lc.bot_username.trim().to_string(),
            ..odoo_cfg.clone()
        };
        Arc::new(
            OdooConnector::connect(&cfg, &credential)
                .await
                .map_err(|e| format!("機器人帳號登入 Odoo 失敗：{e}"))?,
        )
    } else {
        let credential = main_credential.clone().ok_or("[odoo] 的連線憑證尚未設定或無法解密")?;
        Arc::new(OdooConnector::connect(odoo_cfg, &credential).await?)
    };
    let bot_uid = bot.uid.ok_or("Not authenticated")?;
    let dialect = rpc::detect_dialect(&bot).await?;
    let (bot_partner_id, bot_name, _) = rpc::bot_identity(&bot).await?;

    // The admin connection is only needed for Odoo 19 presence writes.
    let admin = if dedicated {
        match main_credential {
            Some(cred) => OdooConnector::connect(odoo_cfg, &cred).await.ok().map(Arc::new),
            None => None,
        }
    } else {
        Some(bot.clone())
    };
    info!(bot_uid, bot_partner_id, ?dialect, "odoo livechat: connected as the bot user");
    Ok(Connected {
        conns: Conns {
            fingerprint,
            instance: super::instance_tag(&odoo_cfg.url, &odoo_cfg.db),
            bot,
            admin,
            dialect,
            bot_uid,
            bot_partner_id,
            presence: None,
            last_heartbeat: None,
        },
        bot_name,
    })
}

async fn heartbeat(c: &mut Conns) -> Result<(), String> {
    if c.last_heartbeat.is_some_and(|t| t.elapsed() < HEARTBEAT_EVERY) {
        return Ok(());
    }
    let api = match c.presence {
        Some(api) => api,
        None => match rpc::heartbeat_bus(&c.bot).await {
            Ok(()) => PresenceApi::Bus,
            Err(e) if rpc::is_missing_bus_presence(&e) => PresenceApi::Mail,
            Err(e) => return Err(format!("無法讓機器人保持上線：{e}")),
        },
    };
    match api {
        // On the very first beat the detection call above already was the
        // `bus.presence` heartbeat; only later beats call it here.
        PresenceApi::Bus if c.presence.is_some() => rpc::heartbeat_bus(&c.bot).await?,
        PresenceApi::Bus => {}
        PresenceApi::Mail => {
            let admin = c.admin.as_ref().ok_or(
                "Odoo 19 需要由 [odoo] 連線帳號（管理員）代為維持機器人上線，但該連線無法使用",
            )?;
            rpc::heartbeat_mail(admin, c.bot_uid).await?;
        }
    }
    if c.presence.is_none() {
        let conn = match api {
            PresenceApi::Bus => c.bot.clone(),
            PresenceApi::Mail => c.admin.clone().unwrap_or_else(|| c.bot.clone()),
        };
        super::set_presence_handle(Some(Arc::new(PresenceHandle {
            conn,
            api,
            bot_uid: c.bot_uid,
        })));
        update_status(|s| {
            s.presence_api = Some(match api {
                PresenceApi::Bus => "bus".into(),
                PresenceApi::Mail => "mail".into(),
            })
        });
    }
    c.presence = Some(api);
    c.last_heartbeat = Some(Instant::now());
    Ok(())
}

async fn go_offline(c: &Conns) {
    if let Some(api) = c.presence {
        let conn = match api {
            PresenceApi::Bus => &c.bot,
            PresenceApi::Mail => c.admin.as_ref().unwrap_or(&c.bot),
        };
        if let Err(e) = rpc::set_offline(conn, api, c.bot_uid).await {
            warn!(error = %e, "odoo livechat: could not mark the bot offline");
        }
    }
    super::set_presence_handle(None);
}

#[allow(clippy::too_many_arguments)]
async fn poll_once(
    home: &Path,
    ctx: &Arc<ReplyContext>,
    c: &Conns,
    lc: &LivechatConfig,
    partner_cache: &mut HashMap<i64, PartnerKind>,
    inflight: &Arc<std::sync::Mutex<HashSet<i64>>>,
    permits: &Arc<Semaphore>,
) -> Result<(), String> {
    let sessions: Vec<Session> = rpc::active_sessions(&c.bot, c.dialect, c.bot_partner_id)
        .await?
        .into_iter()
        .filter(|s| lc.serves_livechat_channel(s.livechat_channel_id))
        .collect();
    let open: Vec<i64> = sessions.iter().map(|s| s.id).collect();
    let cursors: HashMap<i64, i64> = with_state(home, |st| {
        if st.bind_instance(&c.instance) {
            info!("odoo livechat: new Odoo instance — previous session state cleared");
        }
        st.retain_open(&open);
        st.sessions.iter().map(|(id, s)| (*id, s.last_id)).collect()
    });

    // Each session from its own cursor (new sessions from the start). A full
    // page simply continues on the next poll: cursors only move forward.
    let wanted: Vec<(i64, i64)> = open
        .iter()
        .map(|id| (*id, cursors.get(id).copied().unwrap_or(0)))
        .collect();
    let messages = rpc::messages_since(&c.bot, &wanted).await?;

    // Resolve what kind of partner each author is (cached; a partner's
    // internal/archived status essentially never flips mid-chat).
    // Members of sessions seen for the first time are classified too: an
    // operator already in the session (invited before a restart or a state
    // reset) must keep the AI silent.
    let fresh_members = sessions
        .iter()
        .filter(|s| !cursors.contains_key(&s.id))
        .flat_map(|s| s.member_partner_ids.iter().copied());
    let unknown: Vec<i64> = messages
        .iter()
        .filter_map(|m| m.author_partner_id)
        .chain(fresh_members)
        .filter(|p| *p != c.bot_partner_id && !partner_cache.contains_key(p))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if !unknown.is_empty() {
        let internal = rpc::internal_partner_ids(&c.bot, &unknown).await?;
        let archived = rpc::archived_partner_ids(&c.bot, &unknown).await?;
        for p in unknown {
            let kind = if internal.contains(&p) {
                PartnerKind::Internal
            } else if archived.contains(&p) {
                PartnerKind::ChatbotPersona
            } else {
                PartnerKind::Other
            };
            partner_cache.insert(p, kind);
        }
    }
    let of_kind = |k: PartnerKind| -> HashSet<i64> {
        partner_cache
            .iter()
            .filter(|(_, v)| **v == k)
            .map(|(p, _)| *p)
            .collect()
    };
    let internal = of_kind(PartnerKind::Internal);
    let chatbot_partners = of_kind(PartnerKind::ChatbotPersona);

    let mut by_session: HashMap<i64, Vec<ChatMessage>> = HashMap::new();
    for m in messages {
        if m.id > cursors.get(&m.session_id).copied().unwrap_or(0) {
            by_session.entry(m.session_id).or_default().push(m);
        }
    }

    let now = chrono::Utc::now();
    for session in sessions {
        if inflight.lock().map(|g| g.contains(&session.id)).unwrap_or(true) {
            continue;
        }
        let first_seen = !cursors.contains_key(&session.id);
        let batch = by_session.remove(&session.id).unwrap_or_default();
        let plan = plan_session(&batch, c.bot_partner_id, &internal, &chatbot_partners, first_seen, now);
        let operator_member = session
            .member_partner_ids
            .iter()
            .any(|p| *p != c.bot_partner_id && internal.contains(p));

        let human = with_state(home, |st| {
            let s = st.sessions.entry(session.id).or_insert_with(|| SessionState {
                first_seen: now.to_rfc3339(),
                visitor_name: sanitize_visitor_name(&session.anonymous_name),
                ..Default::default()
            });
            if let Some(max) = plan.max_id {
                s.last_id = s.last_id.max(max);
            }
            if plan.operator_spoke && !s.human {
                s.human = true;
                s.human_reason = Some("operator_spoke".into());
                info!(session = session.id, "odoo livechat: a human operator took over");
            } else if first_seen && operator_member && !s.human {
                s.human = true;
                s.human_reason = Some("operator_member".into());
                info!(session = session.id, "odoo livechat: an operator is already in the session — AI stays silent");
            }
            if plan.max_id.is_some() {
                s.last_activity = now.to_rfc3339();
            }
            s.human
        });
        if human {
            continue;
        }
        let greet = first_seen && !lc.greeting.trim().is_empty() && !plan.bot_spoke;
        if plan.visitor.is_empty() && !greet {
            continue;
        }

        if let Ok(mut g) = inflight.lock() {
            g.insert(session.id);
        }
        let input = TurnInput {
            instance: c.instance.clone(),
            session,
            visitor: plan.visitor,
            earlier: plan.earlier,
            greet,
            bot_partner_id: c.bot_partner_id,
        };
        let (home, ctx, conn, lc, inflight, permits) = (
            home.to_path_buf(),
            ctx.clone(),
            c.bot.clone(),
            lc.clone(),
            inflight.clone(),
            permits.clone(),
        );
        let internal = internal.clone();
        tokio::spawn(async move {
            // Released on every exit, a panicking turn included, so the
            // session can never stay locked out of later polls.
            let _inflight = InflightGuard { set: inflight, id: input.session.id };
            let _permit = permits.acquire_owned().await;
            turn::run(&home, &ctx, &conn, &lc, input, &internal).await;
        });
    }
    Ok(())
}

struct InflightGuard {
    set: Arc<std::sync::Mutex<HashSet<i64>>>,
    id: i64,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut g = match self.set.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        g.remove(&self.id);
    }
}

/// The visitor's display name as Odoo reports it. A guest can rename
/// themselves, so it is visitor-controlled: keep it short and strip what
/// could pass for the bridge's own context markers or a new line.
pub(super) fn sanitize_visitor_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '[' | ']' | '［' | '］' | '｜' | '|'))
        .collect();
    duduclaw_core::truncate_chars(cleaned.trim(), 40).to_string()
}

/// What one poll found in one session.
#[derive(Debug, Default, PartialEq)]
pub(super) struct SessionPlan {
    pub max_id: Option<i64>,
    pub operator_spoke: bool,
    pub bot_spoke: bool,
    /// Visitor messages still waiting for an answer.
    pub visitor: Vec<ChatMessage>,
    /// Chatbot-script lines and the visitor lines answered by the script,
    /// before the pending ones (context for a forwarded session's first turn).
    pub earlier: Vec<(Author, ChatMessage)>,
}

/// Pure: decide what to do with a session's new messages. Visitor messages
/// count as unanswered only after the last bot/operator message in the
/// batch; on first sight, ones older than [`STALE_AFTER_MINUTES`] are
/// dropped.
pub(super) fn plan_session(
    batch: &[ChatMessage],
    bot_partner_id: i64,
    internal: &HashSet<i64>,
    chatbot_partners: &HashSet<i64>,
    first_seen: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> SessionPlan {
    let mut plan = SessionPlan {
        max_id: batch.iter().map(|m| m.id).max(),
        ..Default::default()
    };
    for m in batch {
        match classify(m, bot_partner_id, internal, chatbot_partners) {
            Author::Bot => {
                plan.bot_spoke = true;
                plan.visitor.clear();
            }
            Author::Operator => {
                plan.operator_spoke = true;
                plan.visitor.clear();
            }
            Author::ChatbotScript => {
                // A script line answers whatever the visitor said before it.
                for v in plan.visitor.drain(..) {
                    plan.earlier.push((Author::Visitor, v));
                }
                plan.earlier.push((Author::ChatbotScript, m.clone()));
            }
            Author::Visitor => plan.visitor.push(m.clone()),
        }
    }
    if first_seen {
        let cutoff = now - chrono::Duration::minutes(STALE_AFTER_MINUTES);
        plan.visitor.retain(|m| {
            chrono::NaiveDateTime::parse_from_str(&m.create_date, "%Y-%m-%d %H:%M:%S")
                .map(|t| t.and_utc() >= cutoff)
                // An unparseable timestamp is not a reason to ignore a visitor.
                .unwrap_or(true)
        });
    }
    plan
}
