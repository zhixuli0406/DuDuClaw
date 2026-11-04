//! Admin-only group retention controls. Group contents are never returned by
//! these configuration methods.
use super::*;
use crate::group_context::{self, GroupConfig};

impl MethodHandler {
    pub(crate) async fn handle_channels_group_context_list(&self, params: Value) -> WsFrame {
        let channel = params.get("channel").and_then(Value::as_str);
        if channel.is_some_and(|c| !group_context::CHANNELS.contains(&c)) {
            return WsFrame::error_response("", "Invalid channel");
        }
        let home = self.home_dir.clone();
        let (groups,known) = match group_context::blocking(move || {
            Ok((group_context::configured_groups(&home)?,group_context::known_groups(&home)?))
        }).await {
            Ok(result) => result,
            Err(error) => return WsFrame::error_response("", &error),
        };
        let groups = groups.into_iter().filter(|g| channel.is_none_or(|c| g.channel==c)).collect::<Vec<_>>();
        let known = known.into_iter().filter(|g| channel.is_none_or(|c| g.channel==c)).collect::<Vec<_>>();
        WsFrame::ok_response("", json!({"success":true,"groups":groups,"known_groups":known}))
    }

    pub(crate) async fn handle_channels_group_context_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let channel = params.get("channel").and_then(Value::as_str).unwrap_or("");
        if let Err(error) = crate::product_policy::require_channel(&self.home_dir,channel) {
            return WsFrame::error_response("", &error);
        }
        let mut input = params.clone();
        if input.get("scope_id").is_none() && channel != "discord" {
            input["scope_id"] = json!("global");
        }
        let config: GroupConfig = match serde_json::from_value(input) {
            Ok(config) => config,
            Err(_) => return WsFrame::error_response("", "Invalid group settings; Discord requires a server ID"),
        };
        if config.channel == "discord" && config.scope_id == "global" {
            return WsFrame::error_response("", "Discord requires a server ID");
        }
        if config.channel != "discord" && config.scope_id != "global" {
            return WsFrame::error_response("", "This channel uses global access settings");
        }
        let home = self.home_dir.clone();
        let actor = ctx.user_id.clone();
        match group_context::blocking(move || {
            group_context::set(&home,&config)?;
            audit_change(&home,&actor,"group_context_set",&config.channel,&config.group_id,
                json!({"max_messages":config.max_messages,"max_age_hours":config.max_age_hours,"scope_id":config.scope_id}));
            Ok(config)
        }).await {
            Ok(config) => WsFrame::ok_response("",json!({"success":true,"group":config})),
            Err(error) => WsFrame::error_response("", &error),
        }
    }

    pub(crate) async fn handle_channels_group_context_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let channel = params.get("channel").and_then(Value::as_str).unwrap_or("");
        let group = params.get("group_id").and_then(Value::as_str).unwrap_or("");
        let home = self.home_dir.clone();
        let actor = ctx.user_id.clone();
        let channel = channel.to_owned(); let group = group.to_owned();
        match group_context::blocking(move || {
            group_context::remove(&home,&channel,&group)?;
            audit_change(&home,&actor,"group_context_remove",&channel,&group,json!({}));
            Ok(())
        }).await {
            Ok(()) => WsFrame::ok_response("",json!({"success":true})),
            Err(error) => WsFrame::error_response("", &error),
        }
    }
}

fn audit_change(home: &Path, actor: &str, action: &str, channel: &str, group: &str, settings: Value) {
    crate::security_autopilot::audit_and_emit(home,&duduclaw_security::audit::AuditEvent::new(
        action,channel,duduclaw_security::audit::Severity::Info,
        json!({"actor":actor,"source":"dashboard","group_id":group,"settings":settings}),
    ));
}
