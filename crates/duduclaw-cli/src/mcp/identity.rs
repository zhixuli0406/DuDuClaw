use super::*;

/// RFC-21 §1: Resolve a `(channel, external_id)` pair to the canonical person
/// behind it via the [`duduclaw_identity::IdentityProvider`] trait.
///
/// G5 (2026-09 feature audit) completed the migration this used to promise:
/// the provider is whatever `config.toml [identity]` selects — wiki cache
/// (default), Notion, or the chained cache→upstream pair — resolved through
/// the one shared `duduclaw_gateway::identity_provider::build_identity_provider`
/// the dashboard RPC and the channel `<sender>` block also use. Selection is
/// fail-safe: an unconfigured or unreachable upstream degrades to the wiki
/// cache rather than erroring.
pub(crate) async fn handle_identity_resolve(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    // No `use duduclaw_identity::IdentityProvider` needed: the builder hands
    // back an `Arc<dyn IdentityProvider>`, whose methods resolve without the
    // trait in scope.
    let channel_str = match args.get("channel").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_error("Missing required parameter: channel"),
    };
    let external_id = match args.get("external_id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_error("Missing required parameter: external_id"),
    };

    let channel = duduclaw_identity::ChannelKind::parse_wire(channel_str);
    let (provider, _label) =
        duduclaw_gateway::identity_provider::build_identity_provider(home_dir).await;

    match provider
        .resolve_by_channel(channel.clone(), external_id)
        .await
    {
        Ok(Some(person)) => {
            // Surface the structured result as JSON. Agents that need a
            // narrative can render it themselves; downstream code can
            // serde_json::from_value back into ResolvedPerson.
            tracing::info!(
                provider = provider.name(),
                channel = %channel.as_wire(),
                caller_agent = caller_agent,
                hit = true,
                "identity_resolve: matched person_id={}",
                person.person_id,
            );
            match serde_json::to_value(&person) {
                Ok(payload) => {
                    let pretty = serde_json::to_string_pretty(&payload)
                        .unwrap_or_else(|_| payload.to_string());
                    tool_text(&format!(
                        "Resolved person via {} provider:\n\n{pretty}",
                        provider.name()
                    ))
                }
                Err(e) => tool_error(&format!("Failed to serialize ResolvedPerson: {e}")),
            }
        }
        Ok(None) => {
            tracing::info!(
                provider = provider.name(),
                channel = %channel.as_wire(),
                caller_agent = caller_agent,
                hit = false,
                "identity_resolve: no match",
            );
            tool_text(&format!(
                "No identity record matched (channel={}, external_id={}) via the '{}' \
                 identity provider; treat as a stranger unless you can resolve them \
                 by other means.",
                channel.as_wire(),
                external_id,
                provider.name(),
            ))
        }
        Err(e) => {
            tracing::warn!(
                provider = provider.name(),
                channel = %channel.as_wire(),
                caller_agent = caller_agent,
                "identity_resolve: provider error: {}",
                e,
            );
            tool_error(&format!("Identity provider error: {e}"))
        }
    }
}
