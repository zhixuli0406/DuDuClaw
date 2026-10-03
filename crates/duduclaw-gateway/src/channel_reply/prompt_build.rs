use super::*;

// ── Helpers ─────────────────────────────────────────────────

/// Build system prompt with progressive skill injection.
///
/// When `compressed_skills` and `active_skills` are available, uses three-layer
/// progressive loading instead of full injection. Otherwise falls back to legacy
/// full injection.
// `citation_ctx` carries `(agent_id, turn_id, session_id)` — when present,
// wiki pages injected into the prompt are recorded into the global
// `CitationTracker` so the prediction-error feedback bus can later attribute
// trust deltas back to the exact pages that influenced this turn.
// `session_id` is the SESSION-scoped budget id used for the per-conversation
// cap (review BLOCKER R2-1). Distinct from `turn_id` which is the per-turn
// drain key.
/// RFC-21 §1 step 4: resolve the sender's identity through the configured
/// [`duduclaw_identity::IdentityProvider`] and format the result as an
/// XML-delimited `<sender>` block ready for prompt injection.
///
/// `session_id` carries the channel as its colon-prefix (`"discord:1234"`,
/// `"line:U..."`, `"telegram:..."`, ...). Unknown channels degrade to
/// [`duduclaw_identity::ChannelKind::Other`] — the resolver still works.
///
/// Returns an empty string when the sender is unknown or any provider error
/// occurs. That matches v1.10.1 behaviour exactly, so this change is safe to
/// land before any concrete upstream provider (Notion / LDAP) is configured.
///
/// G5 (2026-09 feature audit): the provider is now whatever `config.toml
/// [identity]` selects, via [`crate::identity_provider::build_identity_provider`].
/// This used to hard-code `WikiCacheIdentityProvider`, so an operator who
/// configured Notion saw the dashboard resolve through it while every agent
/// still read only the wiki cache. Selection is fail-safe (see that module),
/// so an unconfigured or unreachable upstream still degrades to the cache.
pub(super) async fn build_sender_block(home_dir: &std::path::Path, session_id: &str, user_id: &str) -> String {
    // No `use duduclaw_identity::IdentityProvider` needed: the builder hands
    // back an `Arc<dyn IdentityProvider>`, whose methods resolve without the
    // trait in scope.
    if user_id.is_empty() {
        return String::new();
    }

    let channel_str = session_id.split(':').next().unwrap_or("unknown");
    let channel = duduclaw_identity::ChannelKind::parse_wire(channel_str);

    let (provider, _label) = crate::identity_provider::build_identity_provider(home_dir).await;
    match provider.resolve_by_channel(channel.clone(), user_id).await {
        Ok(Some(person)) => {
            // Format as a tightly-bounded XML block; agents are trained to
            // treat XML tags as ground-truth context the user cannot
            // override (matches the security-hooks injection-resistance
            // convention used elsewhere in DuDuClaw).
            let mut block = String::with_capacity(256);
            block.push_str("<sender>\n");
            block.push_str(&format!(
                "  <person_id>{}</person_id>\n",
                xml_escape(&person.person_id)
            ));
            block.push_str(&format!(
                "  <display_name>{}</display_name>\n",
                xml_escape(&person.display_name)
            ));
            if !person.roles.is_empty() {
                block.push_str(&format!(
                    "  <roles>{}</roles>\n",
                    xml_escape(&person.roles.join(", "))
                ));
            }
            if !person.project_ids.is_empty() {
                block.push_str(&format!(
                    "  <project_ids>{}</project_ids>\n",
                    xml_escape(&person.project_ids.join(", "))
                ));
            }
            block.push_str(&format!(
                "  <channel>{}</channel>\n",
                xml_escape(&channel.as_wire())
            ));
            block.push_str(&format!(
                "  <source>{}</source>\n",
                xml_escape(provider.name())
            ));
            block.push_str("</sender>");
            block
        }
        Ok(None) => String::new(),
        Err(e) => {
            tracing::warn!(
                provider = provider.name(),
                channel = %channel.as_wire(),
                "build_sender_block: identity provider error: {}",
                e,
            );
            String::new()
        }
    }
}

/// XML escape — keeps `<sender>` block well-formed even if a person record
/// contains `<`, `&`, or quote characters.
pub(super) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

pub(super) fn build_system_prompt(
    agent: Option<&duduclaw_agent::registry::LoadedAgent>,
    user_message: Option<&str>,
    compressed_skills: Option<&[crate::skill_lifecycle::compression::CompressedSkill]>,
    active_skills: Option<&std::collections::HashSet<String>>,
    skill_token_budget: u32,
    team_members: Option<&[TeamMember]>,
    pinned_instructions: &str,
    citation_ctx: Option<(&str, &str, Option<&str>)>,
    // RFC-21 §1: when the IdentityProvider resolved the message sender, the
    // formatted `<sender>...</sender>` XML block is passed in here. Empty
    // string means "no resolution" — agents fall back to treating the sender
    // as a stranger, which matches v1.10.1 behaviour exactly.
    sender_block: &str,
    // P3-2 context-collapse: `true` when this turn is a 1:1 private session,
    // so Personal-or-higher `.scope.toml` wiki namespaces may be injected.
    // `false` (a group/shared session) withholds them. Fail-closed by the
    // caller — see `duduclaw_core::is_private_session`.
    allow_personal: bool,
    // `config.toml [general] default_language`, when set — see
    // `crate::prompt_identity` for the rationale. `None` preserves the
    // pre-existing "follow the user's input language" behaviour.
    default_language: Option<&str>,
) -> String {
    // #11 (2026-05-12) — Minimal mode: short-circuit to the lean assembler.
    // Agents opt in via `agent.toml [prompt] mode = "minimal"`. See
    // commercial/docs/TODO-runtime-health-fixes-202605.md #11.
    if let Some(a) = agent {
        if a.config.prompt.mode == duduclaw_core::types::PromptMode::Minimal {
            return crate::prompt_minimal::build_minimal_system_prompt(
                a,
                sender_block,
                pinned_instructions,
                default_language,
            );
        }
    }

    let mut parts = Vec::new();
    // Mirror parts with labelled byte counts for the prompt-size audit log.
    // See `crate::prompt_audit` — emitted only when total exceeds the
    // 50KB threshold so it stays silent on normal traffic but lights up
    // exactly the requests that risk hitting the 200K cliff.
    let mut audit: Vec<crate::prompt_audit::PromptSection> = Vec::new();

    // Authoritative identity + global default language, ahead of SOUL so it
    // wins over any stale name/language text SOUL.md still contains (see
    // `crate::prompt_identity` doc comment for the root-cause writeup).
    // `agent = None` (no agent resolved) still allows a language-only
    // directive through — the identity half naturally no-ops on an empty name.
    let display_name = agent
        .map(|a| {
            if a.config.agent.display_name.trim().is_empty() {
                a.config.agent.name.as_str()
            } else {
                a.config.agent.display_name.as_str()
            }
        })
        .unwrap_or("");
    if let Some(s) =
        crate::prompt_identity::identity_and_language_section(display_name, default_language)
    {
        audit.push(crate::prompt_audit::PromptSection::new(
            "identity_directive",
            &s,
        ));
        parts.push(s);
    }

    if let Some(a) = agent {
        if let Some(soul) = &a.soul {
            audit.push(crate::prompt_audit::PromptSection::new("soul", soul));
            parts.push(soul.clone());
        }
        if let Some(identity) = &a.identity {
            audit.push(crate::prompt_audit::PromptSection::new(
                "identity", identity,
            ));
            parts.push(identity.clone());
        }

        // RFC-22 P1-9a / P1-8: inject CONTRACT.toml boundaries (must_not /
        // must_always) into the channel system prompt. runner.rs already
        // injects this for sub-agent dispatch but channel_reply did not,
        // which is why 5/5 agnes hallucinated a PM section after pm spawn
        // failed — there was no rule visible to LLM forbidding proxy authoring.
        let contract_prompt = duduclaw_agent::contract::contract_to_prompt(&a.contract);
        if !contract_prompt.is_empty() {
            audit.push(crate::prompt_audit::PromptSection::new(
                "contract",
                &contract_prompt,
            ));
            parts.push(contract_prompt);
        }

        // WP1.3 hardening (2026-07-28): the 📎DELIVER protocol used to live
        // only in the office SKILL.md files, so a model that skipped skill
        // content wrote its .docx to ~/Desktop and never emitted the marker —
        // the gateway then had nothing to send or archive. The rule is now
        // always-on, static text (prompt-cache friendly), runtime-agnostic.
        let deliver_rules = "## 檔案交付規則（強制）\n\
            如果本次回覆產出任何檔案（docx/xlsx/pptx/pdf 等）：\n\
            1. 檔案必須儲存在你的工作目錄（目前所在目錄）內，禁止寫到 ~/Desktop、/tmp 或其他外部路徑。\n\
            2. 回覆最後把每個產出檔各自獨立一行標出：📎DELIVER:<絕對路徑>\n\
            3. 沒有 📎DELIVER 標記，使用者就收不到檔案——只在文字裡描述檔案位置不算交付。";
        audit.push(crate::prompt_audit::PromptSection::new(
            "deliver_rules",
            deliver_rules,
        ));
        parts.push(deliver_rules.to_string());

        // Interaction-pacing rule (2026-07-28 field report): after a heavy
        // task turn, a bare greeting re-triggered minutes of Drive searches —
        // the model treated unfinished history as a standing work order.
        // Always-on static text (prompt-cache friendly, runtime-agnostic; the
        // Direct API path gets real turn structure but the same bias).
        let pacing_rules = "## 互動節奏（強制）\n\
            只回應使用者這一次說的話。寒暄、道謝、簡短閒聊——直接簡短回覆，\
            不呼叫任何工具。先前對話中的任務一律視為已結束或暫停：\
            除非使用者現在明確要求繼續，否則不得自行重啟、續作或補做。";
        audit.push(crate::prompt_audit::PromptSection::new(
            "pacing_rules",
            pacing_rules,
        ));
        parts.push(pacing_rules.to_string());

        // Progressive skill injection (when available)
        let mut skills_total_bytes: usize = 0;
        if let (Some(skills), Some(msg)) = (compressed_skills, user_message) {
            if !skills.is_empty() {
                let mut active = active_skills.cloned().unwrap_or_default();

                // WP1.2 deterministic boost: when the message carries an office
                // document attachment (docx/xlsx/pptx/pdf/csv…), force the
                // matching skill into the active set so `select_layers` promotes
                // its full content to Layer 2. Zero cost when no doc is attached
                // (empty result → `active` unchanged). Only boosts skills the
                // agent actually has loaded.
                for skill_name in crate::office_docs::skills_for_attachment_refs(msg) {
                    if skills.iter().any(|s| s.name == skill_name) {
                        active.insert(skill_name.to_string());
                    }
                }

                // Layer 0: all skill names
                let index: Vec<&str> = skills.iter().map(|s| s.tag.as_str()).collect();
                let s = format!("Available skills: {}", index.join(", "));
                skills_total_bytes += s.len();
                parts.push(s);

                // Rank and select layers
                let ranked = crate::skill_lifecycle::relevance::rank_skills(msg, skills);
                let config = crate::skill_lifecycle::relevance::RelevanceConfig::default();
                let selection = crate::skill_lifecycle::relevance::select_layers(
                    &ranked, &active, skills, &config,
                );

                let mut remaining_budget = skill_token_budget;

                // Layer 2: active + highly relevant — full content
                for &idx in &selection.layer2 {
                    let skill = &skills[idx];
                    if remaining_budget >= skill.tokens_layer2 {
                        let s = format!("## Skill: {}\n{}", skill.name, skill.full_content);
                        skills_total_bytes += s.len();
                        parts.push(s);
                        remaining_budget = remaining_budget.saturating_sub(skill.tokens_layer2);
                    }
                }

                // Layer 1: relevant — summary only
                for &idx in &selection.layer1 {
                    let skill = &skills[idx];
                    if remaining_budget >= skill.tokens_layer1 {
                        let s = format!("## {}: {}", skill.name, skill.summary);
                        skills_total_bytes += s.len();
                        parts.push(s);
                        remaining_budget = remaining_budget.saturating_sub(skill.tokens_layer1);
                    }
                }
            }
        } else {
            // Legacy: inject all skills fully (backward compat when
            // progressive not enabled). #6.2b: cap at
            // DEFAULT_LEGACY_SKILL_BYTE_CAP so an unbounded SKILLS/ dir
            // can't push the prompt past the 200K cliff. Truncation
            // footer (when triggered) explains what was dropped and
            // points operators at progressive injection.
            let pairs: Vec<(String, String)> = a
                .skills
                .iter()
                .map(|s| (s.name.clone(), s.content.clone()))
                .collect();
            let (rendered, footer) = crate::prompt_audit::budgeted_legacy_skills(
                &pairs,
                crate::prompt_audit::DEFAULT_LEGACY_SKILL_BYTE_CAP,
            );
            for s in rendered {
                skills_total_bytes += s.len();
                parts.push(s);
            }
            if let Some(note) = footer {
                skills_total_bytes += note.len();
                parts.push(note);
            }
        }
        if skills_total_bytes > 0 {
            audit.push(crate::prompt_audit::PromptSection {
                label: "skills",
                bytes: skills_total_bytes,
            });
        }
    }

    // RFC-21 §1: inject the `<sender>` block so SOUL.md rules like
    // "reject non-project members" become evaluable from data the agent
    // already has, instead of requiring a mid-reasoning shared-wiki read
    // lookup. XML-delimited per the security-hooks injection-resistance
    // convention — the block is placed before team / wiki context so the
    // agent reads "who am I talking to" before "what do I know".
    if !sender_block.is_empty() {
        audit.push(crate::prompt_audit::PromptSection::new(
            "sender",
            sender_block,
        ));
        parts.push(sender_block.to_string());
    }

    // Inject sub-agent team roster so the agent knows its organizational context.
    // This enables natural delegation: "請團隊檢查" → agent knows which sub-agents to use.
    if let Some(members) = team_members {
        if !members.is_empty() {
            let mut team_section = String::from(
                "## Your Team\nYou have the following sub-agents. Use `spawn_agent` or `send_to_agent` MCP tools to delegate tasks to them.\n",
            );
            for m in members {
                team_section.push_str(&format!(
                    "- **{}** ({}) — {}\n",
                    m.display_name, m.name, m.role
                ));
            }
            audit.push(crate::prompt_audit::PromptSection::new(
                "team",
                &team_section,
            ));
            parts.push(team_section);
        }
    }

    // Wiki knowledge injection — L0 (Identity) + L1 (Core) pages are always
    // injected so the agent can reference accumulated wiki knowledge without
    // manual wiki_search calls. L2/L3 are search-only.
    //
    // #14 glue (2026-05-12): when we have the user_message, rank pages by
    // TF-IDF relevance and keep top-K under the 6 KB budget instead of
    // dumping in file order. The empty-query path falls back to file
    // order via `relevance_ranker`'s fast path, matching prior behaviour.
    if let Some(a) = agent {
        let wiki_dir = a.dir.join("wiki");
        if wiki_dir.exists() {
            let store = duduclaw_memory::WikiStore::new(wiki_dir);
            let query = user_message.unwrap_or("");
            // Hoist the Arc<CitationTracker> binding so the borrow lives
            // long enough for the CitationContext. The tracker itself is
            // a global singleton — cheap to clone the Arc.
            let tracker_arc = citation_ctx.map(|_| duduclaw_memory::feedback::global_tracker());
            let citation_context = citation_ctx.zip(tracker_arc.as_ref()).map(
                |((agent_id, conv_id, session_id), tracker)| {
                    crate::ranked_wiki_injection::CitationContext {
                        agent_id,
                        conversation_id: conv_id,
                        session_id,
                        tracker: tracker.as_ref(),
                    }
                },
            );
            // Session-stable selection: pin the kept-page set per
            // (agent, session) so the wiki section bytes don't churn
            // every turn and break the prompt-cache prefix. Falls back
            // to per-turn ranking when no session identity is known.
            // P3-2: fold the private/shared bit into the cache key so a
            // session's chat type never serves the other type's cached
            // (stripped vs full) page selection.
            let cache_key = citation_ctx.map(|(agent_id, conv_id, session_id)| {
                let priv_tag = if allow_personal { "p" } else { "g" };
                format!("{agent_id}:{}:{priv_tag}", session_id.unwrap_or(conv_id))
            });
            // WP7: department-scope the injection so a `departments/<dept>/`
            // page never reaches an agent outside that department.
            let viewer_department = {
                let d = a.config.agent.department.trim();
                if !d.is_empty() && duduclaw_core::is_valid_department(d) {
                    Some(d.to_string())
                } else {
                    None
                }
            };
            let wiki_ctx = crate::ranked_wiki_injection::ranked_wiki_injection(
                &store,
                query,
                6000,
                citation_context,
                cache_key.as_deref(),
                viewer_department.as_deref(),
                allow_personal,
            );
            // The helper returns "" on error or no pages — wrap the
            // non-empty case identically to before so prompt shape
            // stays stable for prompt-cache hits.
            if !wiki_ctx.is_empty() {
                // CACHE_SPLIT_MARKER: on the Direct API path the wiki
                // section starts a second cached system block, so a wiki
                // change invalidates only this block, not the static
                // SOUL/skills/team prefix. CLI spawn paths strip the
                // marker before writing the system-prompt file.
                let s = format!(
                    "{}\n## Wiki Knowledge\n{}",
                    crate::direct_api::CACHE_SPLIT_MARKER,
                    wiki_ctx.trim_end()
                );
                audit.push(crate::prompt_audit::PromptSection::new("wiki", &s));
                parts.push(s);
            }
        }
    }

    // Instruction Pinning: inject at the END of system prompt (Anthropic best practice:
    // "put instructions at the bottom for best attention"). This combats U-shaped
    // attention degradation by placing key task requirements in the high-attention tail.
    if !pinned_instructions.is_empty() {
        let s = format!(
            "## Pinned Task Instructions\n\
             The user's core task requirements (ALWAYS follow these throughout the conversation):\n\
             {pinned_instructions}"
        );
        audit.push(crate::prompt_audit::PromptSection::new("pinned", &s));
        parts.push(s);
    }

    let agent_label = agent
        .map(|a| a.config.agent.name.as_str())
        .unwrap_or("unknown");
    crate::prompt_audit::maybe_log_breakdown(
        agent_label,
        "channel_reply",
        &audit,
        crate::prompt_audit::DEFAULT_EMIT_THRESHOLD_BYTES,
    );

    if parts.is_empty() {
        "You are DuDuClaw, a helpful AI assistant. Reply concisely in the user's language."
            .to_string()
    } else {
        parts.join("\n\n---\n\n")
    }
}

