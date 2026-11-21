//! RM-191: context of the inbound prompt-injection scan in
//! `build_reply_with_session_inner` — how far the writer is trusted, what
//! the audit record names, and the fixed notice a blocked sender receives.
//! Every other `input_guard` caller stays strict.
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_security::input_guard::ScanTrust;

/// The legacy token principal (already-authenticated admin token, not a
/// person): never treated as a verified member.
const LEGACY_SYSTEM_PRINCIPAL: &str = "system";

/// Trust for the inbound scan, from signals the host resolved itself.
/// [`ScanTrust::VerifiedMemberOwnConversation`] ONLY when the session is not
/// public and either
/// - the requester is a signed-in dashboard account (non-empty, not a
///   widget visitor, not the legacy `"system"` token) in their own WebChat
///   conversation (`own_webchat_conversation`, from
///   `scheduled_webchat::turn_origin`), or
/// - the requester is a messaging sender with a verified binding
///   (`ChannelUser { user_id: Some(_) }`) in a one-to-one conversation
///   (`one_to_one`).
///
/// Anything else — unknown or system requesters, visitors, unbound senders,
/// group chats, calls outside a guarded reply — is [`ScanTrust::Strict`].
/// A later change will OR-in an attested Google Chat DM signal here.
pub(super) fn inbound_guard_trust(
    requester: &RequesterIdentity,
    public: bool,
    own_webchat_conversation: bool,
    one_to_one: bool,
) -> ScanTrust {
    if public {
        return ScanTrust::Strict;
    }
    let verified = match requester {
        RequesterIdentity::DashboardUser { user_id } => {
            !user_id.is_empty()
                && user_id != LEGACY_SYSTEM_PRINCIPAL
                && !crate::webchat::is_widget_visitor(user_id)
                && own_webchat_conversation
        }
        RequesterIdentity::ChannelUser { user_id: Some(bound), .. } => !bound.is_empty() && one_to_one,
        _ => false,
    };
    if verified {
        ScanTrust::VerifiedMemberOwnConversation
    } else {
        ScanTrust::Strict
    }
}

/// [`inbound_guard_trust`] for the current turn: the requester the guarded
/// reply scoped (`Unknown` outside one), the session's public flag, the
/// WebChat origin and the 1:1 test. `user_id` is the channel sender; a
/// bound requester must be that same sender.
pub(super) fn inbound_guard_trust_for_turn(session_id: &str, user_id: &str, agent_id: &str) -> ScanTrust {
    let requester = crate::knowledge::turn_requesters::current_requester();
    let same_sender = matches!(&requester, RequesterIdentity::ChannelUser { sender, .. } if sender == user_id);
    inbound_guard_trust(
        &requester,
        super::assembly::public_session(session_id),
        !crate::scheduled_webchat::turn_origin(session_id, agent_id).is_empty(),
        same_sender && duduclaw_core::is_private_session(session_id, user_id),
    )
}

/// Channel recorded in the audit row: the session id's prefix (a label for
/// the record, not a routing decision).
pub(super) fn audit_channel(session_id: &str) -> &str {
    session_id.split_once(':').map_or("unknown", |(channel, _)| channel)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lang {
    ZhTw,
    ZhCn,
    En,
    Ja,
}

/// Same mapping as the fixed turn-failure notices (`failure_notice.rs`):
/// default zh-TW.
fn lang(language: Option<&str>) -> Lang {
    let language = language.unwrap_or("zh-TW");
    if matches!(language, "en" | "en_US") || language.starts_with("en-") {
        Lang::En
    } else if matches!(language, "ja" | "ja-JP" | "ja_JP") {
        Lang::Ja
    } else if matches!(language, "zh-CN" | "zh_CN") {
        Lang::ZhCn
    } else {
        Lang::ZhTw
    }
}

/// Plain-words label of a matched rule (never the rule name itself).
fn rule_label(rule: &str, lang: Lang) -> &'static str {
    use Lang::*;
    match (rule, lang) {
        ("tool_abuse", ZhTw) => "可直接執行的指令或程式執行語法",
        ("tool_abuse", ZhCn) => "可直接执行的命令或程序执行语法",
        ("tool_abuse", En) => "commands or code-execution syntax that could be run directly",
        ("tool_abuse", Ja) => "そのまま実行できるコマンドやコード実行の構文",
        ("instruction_override", ZhTw) => "要求忽略或改寫 AI 原有指示的內容",
        ("instruction_override", ZhCn) => "要求忽略或改写 AI 原有指示的内容",
        ("instruction_override", En) => "a request to ignore or replace the AI's instructions",
        ("instruction_override", Ja) => "AI の元の指示を無視・変更させる内容",
        ("system_prompt_extraction", ZhTw) => "要求顯示 AI 內部設定的內容",
        ("system_prompt_extraction", ZhCn) => "要求显示 AI 内部设置的内容",
        ("system_prompt_extraction", En) => "a request to reveal the AI's internal setup",
        ("system_prompt_extraction", Ja) => "AI の内部設定を表示させる内容",
        ("role_hijack", ZhTw) => "要求 AI 改變身分或解除限制的內容",
        ("role_hijack", ZhCn) => "要求 AI 改变身份或解除限制的内容",
        ("role_hijack", En) => "a request for the AI to change its role or drop its limits",
        ("role_hijack", Ja) => "AI の役割を変えたり制限を外したりさせる内容",
        ("encoding_bypass", ZhTw) => "要求解碼或轉換編碼的內容",
        ("encoding_bypass", ZhCn) => "要求解码或转换编码的内容",
        ("encoding_bypass", En) => "a request to decode or re-encode hidden text",
        ("encoding_bypass", Ja) => "デコードやエンコード変換を求める内容",
        ("data_exfiltration", ZhTw) => "要求把資料傳送到外部的內容",
        ("data_exfiltration", ZhCn) => "要求把数据发送到外部的内容",
        ("data_exfiltration", En) => "a request to send data to an outside address",
        ("data_exfiltration", Ja) => "データを外部へ送信させる内容",
        ("termination_manipulation", ZhTw) => "要求 AI 永不停止工作的內容",
        ("termination_manipulation", ZhCn) => "要求 AI 永不停止工作的内容",
        ("termination_manipulation", En) => "a request for the AI never to stop working",
        ("termination_manipulation", Ja) => "AI に作業を止めさせない内容",
        ("authority_escalation", ZhTw) => "要求 AI 使用比你更高權限的內容",
        ("authority_escalation", ZhCn) => "要求 AI 使用比你更高权限的内容",
        ("authority_escalation", En) => "a request for the AI to act with more authority than yours",
        ("authority_escalation", Ja) => "あなたより高い権限で AI に動かせる内容",
        ("memory_poisoning", ZhTw) => "要求 AI 永久記住特定權限的內容",
        ("memory_poisoning", ZhCn) => "要求 AI 永久记住特定权限的内容",
        ("memory_poisoning", En) => "a request for the AI to permanently remember a permission",
        ("memory_poisoning", Ja) => "特定の権限を AI に永久に記憶させる内容",
        ("role_provenance", ZhTw) => "偽裝成系統或工具訊息的內容",
        ("role_provenance", ZhCn) => "伪装成系统或工具消息的内容",
        ("role_provenance", En) => "text posing as a system or tool message",
        ("role_provenance", Ja) => "システムやツールのメッセージを装った内容",
        ("action_binding", ZhTw) => "把一次核准挪用到其他動作的內容",
        ("action_binding", ZhCn) => "把一次批准挪用到其他操作的内容",
        ("action_binding", En) => "a request to reuse one approval for a different action",
        ("action_binding", Ja) => "一度の承認を別の操作に流用させる内容",
        ("unicode_injection", ZhTw) => "大量隱藏字元",
        ("unicode_injection", ZhCn) => "大量隐藏字符",
        ("unicode_injection", En) => "many hidden characters",
        ("unicode_injection", Ja) => "多数の不可視文字",
        (_, ZhTw) => "其他可疑內容",
        (_, ZhCn) => "其他可疑内容",
        (_, En) => "other suspicious content",
        (_, Ja) => "その他の疑わしい内容",
    }
}

/// The fixed notice a blocked inbound message gets. Never contains the
/// blocked text, a score or a rule name. Members (non-public sessions) learn
/// that the message did not reach the AI, the categories in plain words and
/// how to share code instead; public visitors get one generic sentence.
pub(super) fn blocked_notice(matched_rules: &[String], language: Option<&str>, public: bool) -> String {
    let lang = lang(language);
    if public {
        return match lang {
            Lang::ZhTw => "這則訊息無法處理，請換個方式描述後再試一次。",
            Lang::ZhCn => "这条消息无法处理，请换个方式描述后再试一次。",
            Lang::En => "This message could not be processed. Please rephrase it and try again.",
            Lang::Ja => "このメッセージは処理できませんでした。表現を変えてもう一度お試しください。",
        }
        .to_string();
    }
    let mut labels: Vec<&str> = Vec::new();
    for rule in matched_rules {
        let label = rule_label(rule, lang);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    if labels.is_empty() {
        labels.push(rule_label("", lang));
    }
    match lang {
        Lang::ZhTw => format!(
            "⚠️ 這則訊息沒有交給 AI 處理，因為系統判斷其中含有風險內容：{}。如果要分享程式碼，請改用附加檔案；如果想執行某個指令，請用文字描述你想做的事。",
            labels.join("、")
        ),
        Lang::ZhCn => format!(
            "⚠️ 这条消息没有交给 AI 处理，因为系统判断其中含有风险内容：{}。如果要分享代码，请改用附加文件；如果想执行某个命令，请用文字描述你想做的事。",
            labels.join("、")
        ),
        Lang::En => format!(
            "⚠️ This message was not passed to the AI because the system treats part of it as risky: {}. To share code, attach it as a file; to run a command, describe what you want to do in words.",
            labels.join("; ")
        ),
        Lang::Ja => format!(
            "⚠️ このメッセージは AI に渡されませんでした。システムが危険とみなす内容が含まれているためです：{}。コードを共有する場合はファイルとして添付し、コマンドを実行したい場合は、やりたいことを文章で説明してください。",
            labels.join("、")
        ),
    }
}

#[cfg(test)]
#[path = "input_guard_gate_tests.rs"]
mod tests;
