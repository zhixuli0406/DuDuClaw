//! What `plan`, `show` and `apply` print (P2-B): ids, layers, counts and
//! human labels — never memory content. Digests stay in the stored plan and
//! the audit trail; the terminal shows channel, time and message number.

use std::collections::BTreeMap;
use std::path::Path;

use duduclaw_gateway::memory_forget_steps as steps;
use duduclaw_memory::{ForgetPlan, SqliteMemoryEngine, source_digest};

/// Everything the printout needs besides the plan itself.
#[derive(Debug, Default, Clone)]
pub(crate) struct Extras {
    /// Collateral source digest → human label (M-3).
    pub labels: BTreeMap<String, String>,
    /// Same-turn assistant replies added to the selection (H-2).
    pub replies: Vec<(i64, i64)>,
    /// Employee-written pages carrying a forgotten source (H-4).
    pub review_pages: Vec<String>,
    /// Ready-to-run plan commands for other namespaces (M-1).
    pub other_namespace_commands: Vec<String>,
}

/// A label for one recorded source: kind, conversation, message, time.
fn source_label(
    home: &Path,
    kind: &str,
    session: &str,
    message: &str,
    seq: Option<i64>,
    observed_at: &str,
) -> String {
    // Shown whole: a cut code could not be told apart from another
    // conversation, nor pasted into `--session`.
    let session_shown = session;
    let when = duduclaw_core::truncate_chars(observed_at, 19).replace('T', " ");
    match (kind, seq) {
        ("channel_message", Some(n)) => {
            let role = steps::message_label(&steps::sessions_db_path(home), session, n)
                .map(|(r, _)| match r.as_str() {
                    "user" => "使用者訊息",
                    "assistant" => "員工回覆",
                    _ => "訊息",
                })
                .unwrap_or("訊息");
            format!("對話 {session_shown} 的{role} #{n}（{when}）")
        }
        ("dispatch_run", _) => format!("排程／派工 {session_shown} 的一次執行（{when}）"),
        ("mcp_turn", _) => format!("對話 {session_shown} 中員工自行存入（{when}）"),
        ("import_item", _) => format!("匯入 {session_shown}（{when}）"),
        ("footprint_day", _) => format!("足跡 {} {}", session_shown, message),
        _ => format!("{kind} {session_shown} {message}（{when}）"),
    }
}

/// Human labels for the plan's collateral sources.
pub(crate) async fn collateral_labels(
    engine: &SqliteMemoryEngine,
    home: &Path,
    p: &ForgetPlan,
) -> BTreeMap<String, String> {
    let wanted: std::collections::HashSet<&str> = p
        .document
        .body
        .collateral
        .iter()
        .map(|c| c.source_digest.as_str())
        .collect();
    if wanted.is_empty() {
        return BTreeMap::new();
    }
    let agent = p.document.agent_id.as_str();
    let mut rows: Vec<(String, String, String, Option<i64>, String)> = Vec::new();
    {
        let conn = engine.conn_for_maintenance().await;
        for t in &p.document.body.targets {
            let Ok(mut stmt) = conn.prepare_cached(
                "SELECT source_kind, source_session, source_message, source_seq, source_observed_at
                 FROM memory_origins WHERE memory_store = ?1 AND memory_id = ?2",
            ) else {
                continue;
            };
            if let Ok(it) = stmt.query_map(rusqlite::params![t.store, t.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            }) {
                rows.extend(it.flatten());
            }
        }
    }
    let mut out = BTreeMap::new();
    for (kind, session, message, seq, at) in rows {
        let d = source_digest(agent, &session, &message, "");
        if wanted.contains(d.as_str()) && !out.contains_key(&d) {
            out.insert(d, source_label(home, &kind, &session, &message, seq, &at));
        }
    }
    out
}

/// Plan commands for the other namespaces that recorded the same
/// conversation (each needs its own plan and approval).
pub(crate) async fn other_namespace_commands(
    engine: &SqliteMemoryEngine,
    p: &ForgetPlan,
) -> Vec<String> {
    let d = &p.document;
    let namespaces: Vec<String> = {
        let conn = engine.conn_for_maintenance().await;
        conn.prepare(
            "SELECT DISTINCT agent_id FROM memory_origins
             WHERE source_session = ?1 AND agent_id <> ?2 ORDER BY agent_id LIMIT 20",
        )
        .and_then(|mut s| {
            s.query_map(rusqlite::params![d.selector.session, d.agent_id], |r| {
                r.get(0)
            })
            .and_then(|it| it.collect())
        })
        .unwrap_or_default()
    };
    let messages = if d.selector.messages.is_empty() {
        String::new()
    } else {
        let nums: Vec<String> = d
            .selector
            .messages
            .iter()
            .map(|m| m.strip_prefix("m:").unwrap_or(m).to_string())
            .collect();
        format!(" --message {}", nums.join(","))
    };
    namespaces
        .into_iter()
        .map(|ns| {
            format!(
                "duduclaw memory forget-source plan --agent {ns} --session '{}'{messages}",
                d.selector.session.replace('\'', "'\\''")
            )
        })
        .collect()
}

/// Human summary of a plan: ids, layers, predicates, counts — no content.
pub(crate) fn render_plan(p: &ForgetPlan, x: &Extras) -> String {
    let b = &p.document.body;
    let mems = b.targets.iter().filter(|t| t.store == "memories").count();
    let facts = b.targets.len() - mems;
    let mut o = format!(
        "計畫 {}（{}，命名空間 {}）\n",
        p.plan_id, p.status, p.document.agent_id
    );
    if b.targets.is_empty() && b.archive_ids.is_empty() {
        // H-1: the source is in the conversation record only.
        o.push_str(
            "沒有可刪的記憶，但套用後會設下之後不再學到的紀錄，並處理下列對話紀錄與頁面。\n",
        );
    } else {
        o.push_str(&format!(
            "將刪除 {mems} 筆記憶、{facts} 筆關鍵事實、{} 份封存副本、{} 個自動建檔頁面。\n",
            b.archive_ids.len(),
            b.wiki_pages.len()
        ));
    }
    if !b.targets.is_empty() {
        o.push_str(
            "（每行：記憶編號、種類、怎麼找到的。「直接」＝這筆記憶本身就來自被忘的來源；\
             「衍生」＝它是從那個來源的其他記憶整理出來的）\n",
        );
    }
    for t in &b.targets {
        let predicate = t
            .predicate
            .as_deref()
            .map(|p| format!("  關係 {p}"))
            .unwrap_or_default();
        o.push_str(&format!(
            "  {}  [{}／{}]{predicate}  {}\n",
            t.id,
            store_label(&t.store),
            layer_label(&t.layer),
            found_label(t)
        ));
    }
    if !x.replies.is_empty() {
        let shown: Vec<String> = x
            .replies
            .iter()
            .map(|(u, r)| format!("#{u} 的回覆 #{r}"))
            .collect();
        o.push_str(&format!(
            "同一輪的員工回覆也一併忘記（回覆可能複述使用者的話）：{}\n",
            shown.join("、")
        ));
    }
    let turns = b.linked_turns.len();
    if turns > 0 {
        o.push_str(&format!(
            "員工在這些對話回合中自行存入的內容視為同一個來源：{turns} 個回合也一併設為不再學到。\n"
        ));
    }
    o.push_str(&format!(
        "套用時會設下 {} 筆防止再學到的紀錄（其中 {turns} 筆是對話回合）。\n",
        b.tombstones.len()
    ));
    if !b.collateral.is_empty() {
        o.push_str("連帶影響：下列其他來源也支撐被刪的記憶，這些記憶會整筆刪除：\n");
        for c in &b.collateral {
            let label = x
                .labels
                .get(&c.source_digest)
                .cloned()
                .unwrap_or_else(|| "另一個來源（紀錄已不完整）".to_string());
            o.push_str(&format!("  {label}：{} 筆\n", c.rows_lost));
        }
    }
    if !b.reaffirm_only.is_empty() {
        o.push_str(&format!(
            "保留，僅移除佐證紀錄：{} 筆記憶只是被這個來源再次提到，記憶本身保留（信心值不回退）。\n",
            b.reaffirm_only.len()
        ));
    }
    if !b.supersession_cuts.is_empty() {
        o.push_str(&format!(
            "取代鏈：{} 處連結會切斷；被刪記憶先前取代的舊版本維持「已失效」，不會恢復成目前值。\n",
            b.supersession_cuts.len()
        ));
    }
    if !b.wiki_pages.is_empty() {
        o.push_str(
            "自動建檔頁面只要仍列著被忘的來源就整頁刪除，即使它之後被別的對話改寫過；\
             頁面只保留最近 10 筆來源，更早的來源若只留在頁面的修訂紀錄裡，比對不到。\n",
        );
    }
    if !x.review_pages.is_empty() {
        o.push_str("需要人工檢視（員工自己寫的頁面，記錄的來源命中；不會自動刪除）：\n");
        for page in &x.review_pages {
            o.push_str(&format!("  {page}\n"));
        }
    }
    if b.review_cards_matching > 0 {
        o.push_str(&format!(
            "審核卡：{} 張會撤回並清除文字。\n",
            b.review_cards_matching
        ));
    }
    if !b.session_messages.is_empty() {
        o.push_str(&format!(
            "對話紀錄：{} 則訊息之後不再給員工看到（原文仍保存在對話紀錄中，要刪原文請另行處理）。\n",
            b.session_messages.len()
        ));
    }
    o.push_str(&summary_line(&p.document.selector));
    o.push_str(&format!(
        "此命名空間另有 {} 筆{UNTRACKED_PHRASE}，本功能不會動它們。\n",
        b.untracked_in_namespace
    ));
    if !x.other_namespace_commands.is_empty() {
        o.push_str("其他命名空間也記錄了同一段對話，不在這次範圍內；要一起忘記請分別執行：\n");
        for c in &x.other_namespace_commands {
            o.push_str(&format!("  {c}\n"));
        }
    } else if b.other_namespaces_referencing > 0 {
        o.push_str(&format!(
            "其他命名空間也有 {} 筆記憶引用同一段對話，不在這次範圍內。\n",
            b.other_namespaces_referencing
        ));
    }
    if b.targets.len() > 5_000 {
        o.push_str("規模較大：套用時會鎖住記憶庫數秒，建議先暫停該員工。\n");
    }
    o.push_str(NOT_COVERED);
    o
}

/// What forget by source does not reach (printed with every plan).
pub(crate) const NOT_COVERED: &str = "不在範圍內：已送出的訊息無法收回；工具呼叫紀錄與錯誤筆記\
    （每一輪都會再放進員工的提示，裡面可能留有原話）、任務看板與 /goal 文字、工作狀態、\
    Agent Mail、目標狀態與判官回饋、交接副本、轉交給其他員工的內容與子員工寫回的回覆、\
    其他種類的審核卡、使用者家目錄下 Claude CLI 自己的對話紀錄、共用 wiki 副本、\
    備份與快照都不受影響；用套用前的備份還原，被忘記的記憶會回來。資料庫層會刪除，\
    磁碟層的物理殘留不保證。員工之後重新得知的內容視為新資訊。\
    使用 Gemini CLI 的員工透過 MCP 存的記憶沒有綁到對話，忘記對話時碰不到；\
    Grok 員工的同類記憶是否綁到對話未經驗證。\
    通道回覆先走本機模型（inference_mode = local）時，員工在那一步透過工具存的記憶\
    沒有綁到對話，忘記對話時碰不到。\
    匯入的檔案以路徑識別：同一個檔案重新匯入會被擋，內容相同但放在另一個路徑的檔案視為新來源。\
    員工直接編輯自己 wiki 檔案時可以移除來源標記，該頁就不會出現在「需要人工檢視」。\
    每次回覆後寫下的強化學習軌跡檔（rl_trajectories.jsonl 與 rl_trajectories/ 目錄，\
    內含整段對話原文）不會刪除：檔案裡沒有能精確對應訊息的編號，請自行處理。\
    計畫很大時，套用會長時間鎖住記憶庫，請先暫停該員工。\
    套用前需要管理員在儀表板核准；與員工共用同一個作業系統使用者、能執行任意指令\
    並刻意繞過檔案守門的員工，仍可能直接改寫本機資料庫，真正的隔離要靠不授予 Bash \
    或使用任務沙箱。\n";

/// What the untracked count counts (plan and "nothing to forget" output).
pub(crate) const UNTRACKED_PHRASE: &str = "沒有完整來源紀錄的記憶（功能上線前的舊記憶，\
    或派工時沒拿到完整上游對話身分的記憶）";

fn store_label(store: &str) -> &str {
    match store {
        "memories" => "記憶",
        "key_facts" => "關鍵事實",
        other => other,
    }
}

fn layer_label(layer: &str) -> &str {
    match layer {
        "semantic" => "語意",
        "episodic" => "事件",
        "key_fact" => "事實",
        other => other,
    }
}

/// How a target was reached, in words.
fn found_label(t: &duduclaw_memory::engine::forget_source::TargetEntry) -> &'static str {
    match (t.via.as_str(), t.direct) {
        ("origins", true) => "直接",
        ("origins", false) => "衍生（繼承了被忘來源的記憶）",
        ("derived_from" | "source_ids", _) => "衍生（由被刪的記憶推導）",
        ("key_facts.source_session" | "metadata.session_id", _) => {
            "直接（舊記憶，依記錄的對話比對）"
        }
        _ => "其他",
    }
}

/// The session-summary line. A summary covers the whole conversation and
/// cannot be cut down to one message or run, so a narrower plan still
/// clears the whole conversation's summary; say so.
fn summary_line(sel: &duduclaw_memory::engine::forget_source::FrozenSelector) -> String {
    if sel.messages.is_empty() {
        return "這段對話的壓縮摘要會被清除，員工會失去較早對話的濃縮脈絡。\n".to_string();
    }
    let what = if sel.messages.iter().all(|m| m.starts_with("run:")) {
        "這次執行"
    } else {
        "這幾則訊息"
    };
    format!(
        "壓縮摘要會清除整段 {} 的摘要：摘要是整段共用一份，無法只拿掉{what}的部分，\
         清掉才能確定被忘的內容不會從摘要回到員工；員工會失去較早對話的濃縮脈絡。\n",
        sel.session
    )
}

/// Human name of a follow-up step.
pub(crate) fn step_label(step: &str) -> &str {
    match step {
        duduclaw_memory::engine::forget_source::STEP_WIKI_PAGE_DELETE => "刪除自動建檔頁面",
        duduclaw_memory::engine::forget_source::STEP_REVIEW_SCRUB => "清除審核卡",
        duduclaw_memory::engine::forget_source::STEP_SESSION_HIDE => "隱藏對話紀錄",
        duduclaw_memory::engine::forget_source::STEP_SESSION_SUMMARY_CLEAR => "清除壓縮摘要",
        other => other,
    }
}

fn part_label(part: &str) -> &str {
    match part {
        "tombstones" => "要設的防復活紀錄",
        "linked_turns" => "連帶的對話回合",
        "reaffirm_only" => "只移除佐證紀錄的記憶",
        "archive_ids" => "封存副本",
        "lineage_only" => "已刪記憶留下的來源紀錄",
        "collateral" => "連帶影響的來源",
        "supersession_cuts" => "取代鏈",
        "entity_embeddings_orphaned" => "實體向量",
        "untracked_in_namespace" => "沒有完整來源紀錄的記憶數",
        "other_namespaces_referencing" => "其他命名空間的引用數",
        "wiki_pages" => "自動建檔頁面",
        "review_cards_matching" => "審核卡",
        "session_messages" => "對話紀錄",
        "steps" => "後續步驟",
        "not_covered" => "不在範圍內的清單",
        other => other,
    }
}

/// Every reason a plan went stale, in words (issue 6: not only the first).
pub(crate) fn stale_detail(reason: &duduclaw_memory::StaleReason) -> String {
    use duduclaw_memory::StaleReason as R;
    let parts: Vec<String> = reason
        .reasons()
        .into_iter()
        .map(|r| match r {
            R::Epoch { .. } => "這個命名空間在計畫之後已被另一次刪除變動過".to_string(),
            R::Changed {
                added,
                removed,
                changed,
                other_parts,
                ..
            } => {
                let mut s = String::from("計畫之後資料有變");
                if added + removed + changed > 0 {
                    s.push_str(&format!(
                        "（記憶新增 {added}、消失 {removed}、內容改變 {changed} 筆）"
                    ));
                }
                if !other_parts.is_empty() {
                    let names: Vec<&str> = other_parts.iter().map(|p| part_label(p)).collect();
                    s.push_str(&format!("，變動的部分：{}", names.join("、")));
                } else if added + removed + changed == 0 {
                    // Same rows, same parts: the plan was written by an older
                    // build whose plan format differs (F9).
                    s.push_str("（計畫是用較舊的版本建立的，格式不同；請重新建立計畫）");
                }
                s
            }
            R::TooLarge { reason } => format!("重算後超過上限（{reason}）"),
            R::AlreadyStale => "計畫先前已判定為過時".to_string(),
            R::Several { .. } => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    parts.join("；")
}
