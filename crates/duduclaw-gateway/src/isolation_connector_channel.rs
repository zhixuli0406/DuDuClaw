//! Exact-account channel adapters. Routes and their credentials are installed by
//! the gateway; a tool argument can only narrow a route, never select a home bot.
use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Arc};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::{RequestContext, VerificationError, tool_policy::AuthorizedToolCall};
use serde_json::{Value, json};
use crate::isolation_connectors::{BoundConnectorExecutor, LiveConnectorSelectionPolicy, ProductConnectorAccount, SelectedConnector};
use crate::isolation_http::LiveRequest;

pub const CHANNEL_TOOLS: &[&str] = &["group_history_read", "send_message", "send_photo", "send_sticker",
    "channel_config", "channel_config_list", "channel_status", "pairing_manage"];

/// No Debug/Serialize: the target may contain a bot credential. The scope is an
/// exact chat, settings scope or pairing subject, never a wildcard.
pub struct ChannelAccount {
    account: ProductConnectorAccount,
    channel: String,
    scope: String,
    status_label: String,
    target: Option<crate::channel_sender::ChannelTarget>,
}
impl ChannelAccount {
    pub fn new(account: ProductConnectorAccount, channel: String, scope: String, status_label: String,
        token: Option<String>, extra_id: Option<String>) -> Result<Self, VerificationError> {
        if !crate::channel_settings::VALID_CHANNEL_TYPES.contains(&channel.as_str()) || scope.is_empty()
            || scope.len() > 512 || scope.chars().any(char::is_control) || scope == "*"
            || !(status_label == channel || status_label.starts_with(&format!("{channel}:"))) {
            return Err(VerificationError::Invalid);
        }
        if token.as_ref().is_some_and(|token| token.is_empty() || token.len()>16384
            || !token.bytes().all(|b|b.is_ascii_alphanumeric() || if channel=="telegram" { matches!(b,b':'|b'_'|b'-'|b'.') }
                else { matches!(b,b':'|b'_'|b'-'|b'.'|b'~'|b'+'|b'/'|b'=') }))
            || extra_id.as_ref().is_some_and(|v|v.len()>512 || v.chars().any(char::is_control) || v.contains(['/', '?', '#'])) {
            return Err(VerificationError::Invalid);
        }
        // These native senders intentionally ignore target.token and read the
        // installed multi-field home credential. They cannot impersonate a
        // Personal account by attaching an unrelated account label to that I/O.
        if (crate::channel_sender::sender_self_configures(&channel)
            || (channel=="feishu" && token.as_ref().is_none_or(|t|t.is_empty())))
            && !matches!(&account,ProductConnectorAccount::SharedHome {..}) {
            return Err(VerificationError::BindingMismatch);
        }
        let target = Some(crate::channel_sender::ChannelTarget {
            channel_type: channel.clone(), chat_id: scope.clone(), token: token.unwrap_or_default(), extra_id,
        });
        Ok(Self { account, channel, scope, status_label, target })
    }
    fn matches(&self, tool: &str, args: &Value) -> bool {
        if args.get("channel").is_some_and(|v| v.as_str() != Some(self.channel.as_str())) { return false; }
        let scope_key = match tool {
            "group_history_read" => "group_id", "send_message" | "send_photo" | "send_sticker" => "chat_id",
            "channel_config" | "channel_config_list" => "scope_id", "pairing_manage" => "subject",
            "channel_status" => return true, _ => return false,
        };
        if tool == "pairing_manage" && args.get("action").and_then(Value::as_str) == Some("list") {
            return args.get(scope_key).is_none_or(|v| v.as_str() == Some(self.scope.as_str()));
        }
        args.get(scope_key).and_then(Value::as_str) == Some(self.scope.as_str())
    }
}

pub struct ChannelConnectorProvider {
    home: PathBuf, agent: String, employee: EmployeeId,
    config: String, agent_config: String, accounts: BTreeMap<String, ChannelAccount>,
    http: reqwest::Client,
}
impl ChannelConnectorProvider {
    pub fn new(home: PathBuf, agent: String, employee: EmployeeId, accounts: Vec<ChannelAccount>) -> Result<Self, VerificationError> {
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(&agent) || accounts.is_empty() || accounts.len() > 128 {
            return Err(VerificationError::Invalid);
        }
        let config = std::fs::read_to_string(home.join("config.toml")).map_err(|_| VerificationError::Unavailable)?;
        let agent_config = effective_agent(&home, &agent)?;
        let mut registry = BTreeMap::new();
        for entry in accounts {
            if registry.insert(entry.account.account().to_owned(), entry).is_some() { return Err(VerificationError::Invalid); }
        }
        let http = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never()).timeout(std::time::Duration::from_secs(30)).build().map_err(|_| VerificationError::Unavailable)?;
        Ok(Self { home, agent, employee, config, agent_config, accounts: registry, http })
    }
    pub fn home(&self) -> &std::path::Path { &self.home }
    pub fn agent(&self) -> &str { &self.agent }
    fn live(&self, tool: &str, route: &ChannelAccount) -> Result<(), VerificationError> {
        if std::fs::read_to_string(self.home.join("config.toml")).ok().as_deref() != Some(&self.config)
            || effective_agent(&self.home, &self.agent)? != self.agent_config {
            return Err(VerificationError::BindingMismatch);
        }
        duduclaw_core::product_profile::load(&self.home).and_then(|p| p.require_channel(&route.channel))
            .map_err(|_| VerificationError::Unavailable)?;
        let agent: toml::Value = toml::from_str(&self.agent_config).map_err(|_| VerificationError::Invalid)?;
        if agent.get("agent").and_then(|a| a.get("status")).and_then(toml::Value::as_str)
            .is_some_and(|s| s != "active") { return Err(VerificationError::Unavailable); }
        let caps = agent.get("capabilities");
        if caps.and_then(|c| c.get("denied_tools")).and_then(toml::Value::as_array)
            .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(tool))) { return Err(VerificationError::Unavailable); }
        if caps.and_then(|c| c.get("allowed_tools")).and_then(toml::Value::as_array)
            .is_some_and(|list| !list.is_empty() && !list.iter().any(|v| v.as_str() == Some(tool))) { return Err(VerificationError::Unavailable); }
        Ok(())
    }
    pub fn prepare(&self, selected: &SelectedConnector, call: &AuthorizedToolCall) -> Result<(), VerificationError> {
        selected.verify_call(call)?;
        let route = self.accounts.get(selected.account()).ok_or(VerificationError::BindingMismatch)?;
        if !route.matches(call.name(), call.arguments()) { return Err(VerificationError::BindingMismatch); }
        if matches!(call.name(),"channel_config"|"channel_config_list") {
            crate::channel_settings::validate_scope_id(&route.scope).map_err(|_|VerificationError::Invalid)?;
        }
        if matches!(call.name(),"send_message"|"send_photo"|"send_sticker") && route.channel=="discord"
            && !crate::channel_sender::is_valid_discord_chat_id(&route.scope) {return Err(VerificationError::Invalid);}
        self.live(call.name(), route)
    }
    async fn run(&self, selected: SelectedConnector, call: AuthorizedToolCall) -> Result<Value, VerificationError> {
        self.prepare(&selected, &call)?;
        if call.name() == "group_history_read" { return Err(VerificationError::Unavailable); }
        let route = self.accounts.get(selected.account()).ok_or(VerificationError::BindingMismatch)?;
        let selected = Arc::new(selected);
        let mut effect_started = false;
        let result = duduclaw_mcp_boundary::io_authority::scope(selected.io_context(), async {
            let args = call.arguments();
            let text = match call.name() {
                "send_message" => {
                    let text = args.get("text").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or(VerificationError::Invalid)?;
                    let target = route.target.as_ref().filter(|t| !t.token.is_empty()
                        || matches!(t.channel_type.as_str(),"googlechat"|"teams"|"wecom"|"dingtalk"))
                        .ok_or(VerificationError::Unavailable)?;
                    selected.recheck()?;
                    effect_started = true;
                    crate::channel_sender::create_sender_for_home(&self.home, target, self.http.clone()).send_text(text).await
                        .map_err(|_| VerificationError::Unavailable)?;
                    "Message sent successfully.".to_owned()
                },
                "send_photo" | "send_sticker" => {
                    let media = args.get("url_or_path").or_else(|| args.get("url")).or_else(|| args.get("sticker_id"))
                        .or_else(|| args.get("file_id")).and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or(VerificationError::Invalid)?;
                    let target = route.target.as_ref().filter(|t| !t.token.is_empty()).ok_or(VerificationError::Unavailable)?;
                    let request = match route.channel.as_str() {
                        "telegram" => {
                            let (method,key) = if call.name() == "send_photo" { ("sendPhoto","photo") } else { ("sendSticker","sticker") };
                            self.http.post(format!("https://api.telegram.org/bot{}/{method}", target.token))
                                .json(&json!({"chat_id":route.scope,key:media}))
                        },
                        "discord" if route.scope.bytes().all(|b| b.is_ascii_digit()) => self.http
                            .post(format!("https://discord.com/api/v10/channels/{}/messages",route.scope))
                            .header("Authorization",format!("Bot {}",target.token)).json(&json!({"content":media})),
                        _ => return Err(VerificationError::Unavailable),
                    };
                    selected.recheck()?;
                    effect_started = true;
                    let response = request.send_authorized().await.map_err(|_| VerificationError::Unavailable)?;
                    if !response.status().is_success() { return Err(VerificationError::Unavailable); }
                    if route.channel == "telegram" {
                        let body: Value = response.json().await.map_err(|_| VerificationError::Unavailable)?;
                        if body.get("ok").and_then(Value::as_bool) != Some(true) { return Err(VerificationError::Unavailable); }
                    }
                    "Media sent successfully.".to_owned()
                },
                "channel_config" | "channel_config_list" => {
                    let mgr = crate::channel_settings::ChannelSettingsManager::from_session_db(&self.home.join("sessions.db"))
                        .map_err(|_| VerificationError::Unavailable)?;
                    if call.name() == "channel_config_list" {
                        selected.recheck()?;
                        let values = mgr.get_all(&route.channel,&route.scope).await;
                        values.into_iter().map(|(k,v)|format!("{k} = {v}")).collect::<Vec<_>>().join("\n")
                    } else {
                        let key = args.get("key").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
                        if !crate::channel_settings::CONFIG_KEYS.contains(&key) && !crate::channel_settings::MCP_ACCESS_KEYS.contains(&key) {
                            return Err(VerificationError::Invalid);
                        }
                        if let Some(value) = args.get("value").and_then(Value::as_str) {
                            crate::channel_settings::validate_setting_value(key,value).map_err(|_| VerificationError::Invalid)?;
                            selected.recheck()?;
                            effect_started = true;
                            mgr.set(&route.channel,&route.scope,key,value).await.map_err(|_| VerificationError::Unavailable)?;
                            format!("Set {}/{}/{key} = {value}",route.channel,route.scope)
                        } else {
                            selected.recheck()?;
                            let value = mgr.get_with_fallback(&route.channel,&route.scope,key,"(not set)").await;
                            format!("{}/{}/{key} = {value}",route.channel,route.scope)
                        }
                    }
                },
                "pairing_manage" => {
                    let ctrl = crate::access_control::AccessController::with_persistence(self.home.join("access_control.json"));
                    selected.recheck()?;
                    let action=args.get("action").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
                    if matches!(action,"generate"|"approve"|"revoke") {effect_started=true;}
                    match action {
                        "generate" => {
                            let code = ctrl.generate_pairing_code(&route.scope).await.ok_or(VerificationError::Unavailable)?;
                            format!("配對碼：{code}（5 分鐘內有效）。請使用者在頻道輸入：/pair {code}\nsubject: {}",route.scope)
                        },
                        "approve" => { ctrl.approve_user(&route.scope).await; format!("已核准：{}",route.scope) },
                        "revoke" => { ctrl.revoke_user(&route.scope).await; format!("已撤銷：{}",route.scope) },
                        "list" => {
                            let users = ctrl.runtime_approved_users().await;
                            json!({"approved":users.into_iter().filter(|s| s == &route.scope).collect::<Vec<_>>()}).to_string()
                        },
                        _ => return Err(VerificationError::Invalid),
                    }
                },
                "channel_status" => {
                    selected.recheck()?;
                    let raw = std::fs::read_to_string(self.home.join("channel_status.json")).map_err(|_| VerificationError::Unavailable)?;
                    let status: Value = serde_json::from_str(&raw).map_err(|_| VerificationError::Unavailable)?;
                    let (total,active,threads) = {
                    let db = rusqlite::Connection::open_with_flags(self.home.join("sessions.db"),rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .map_err(|_|VerificationError::Unavailable)?;
                    selected.recheck()?;
                    let mut query=db.prepare("SELECT id,last_active FROM sessions WHERE id=?1 OR id=?2")
                        .map_err(|_|VerificationError::Unavailable)?;
                    let records=query.query_map(rusqlite::params![format!("{}:{}",route.channel,route.scope),format!("{}:thread:{}",route.channel,route.scope)],
                        |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).map_err(|_|VerificationError::Unavailable)?;
                    let mut total=0usize;let mut active=0usize;let mut threads=0usize;
                    let cutoff=chrono::Utc::now()-chrono::Duration::hours(24);
                    for row in records {
                        let (id,last)=row.map_err(|_|VerificationError::Unavailable)?;
                        total+=1;
                        if chrono::DateTime::parse_from_rfc3339(&last).is_ok_and(|t|t.with_timezone(&chrono::Utc)>cutoff) {active+=1;}
                        if id.starts_with("discord:thread:") {threads+=1;}
                    }
                    (total,active,threads)
                    };
                    let mut guilds=Vec::new();
                    if route.channel=="discord" && route.scope!="dm" {
                        let mgr=crate::channel_settings::ChannelSettingsManager::from_session_db(&self.home.join("sessions.db")).map_err(|_|VerificationError::Unavailable)?;
                        let settings:serde_json::Map<String,Value>=mgr.get_all("discord",&route.scope).await.into_iter().map(|(k,v)|(k,Value::String(v))).collect();
                        guilds.push(json!({"guild_id":route.scope,"settings":settings}));
                    }
                    json!({"connections":{"channels":{route.status_label.clone():status.get("channels").and_then(|s|s.get(&route.status_label))},
                        "updated_at":status.get("updated_at")},"sessions":{route.channel.clone():{"total_sessions":total,"active_24h":active,"thread_sessions":threads}},
                        "discord_guilds":guilds}).to_string()
                },
                _ => return Err(VerificationError::Unavailable),
            };
            selected.recheck()?;
            Ok(json!({"content":[{"type":"text","text":text}]}))
        }).await;
        if result.is_err() && effect_started {
            return Ok(json!({"isError":true,"content":[{"type":"text","text":
                "無法確認頻道操作的結果；若有寫入或訊息，請先到原服務檢查，勿直接重試。"}]}));
        }
        result
    }
}
fn effective_agent(home: &std::path::Path, agent: &str) -> Result<String, VerificationError> {
    let dir = crate::ephemeral::resolve_agent_dir(home,agent).or_else(|| (!crate::ephemeral::is_ephemeral_id(agent))
        .then(||home.join("agents").join(agent))).ok_or(VerificationError::Unavailable)?;
    duduclaw_core::agent_toml::read_effective_text(&dir).map_err(|_| VerificationError::Unavailable)
}
impl LiveConnectorSelectionPolicy for ChannelConnectorProvider {
    fn supports(&self, tool: &str) -> bool { CHANNEL_TOOLS.contains(&tool) }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str) -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if provider != "channel" || context.record().employee != self.employee || !CHANNEL_TOOLS.contains(&tool) {
            return Err(VerificationError::Unavailable);
        }
        Ok(self.accounts.values().filter(|r| self.live(tool,r).is_ok()).map(|r|r.account.clone()).collect())
    }
    fn candidates_for_call(&self, context: &RequestContext, tool: &str, provider: &str, args: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        Ok(self.candidates(context,tool,provider)?.into_iter().filter(|a| self.accounts.get(a.account()).is_some_and(|r|r.matches(tool,args))).collect())
    }
}
impl BoundConnectorExecutor for ChannelConnectorProvider {
    fn supports(&self, tool: &str) -> bool { CHANNEL_TOOLS.contains(&tool) && tool != "group_history_read" }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output=Result<Value, VerificationError>>+Send+'a>> { Box::pin(self.run(selected,call)) }
}

#[cfg(test)]
#[path="isolation_connector_channel_tests.rs"]
mod tests;
