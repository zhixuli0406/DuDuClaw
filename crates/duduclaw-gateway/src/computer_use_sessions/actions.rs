//! Parameter validation and the risk gate for tool-driven actions (design
//! §3.4 steps 1 and 3). Pure, so every branch is unit-tested.

use serde::Deserialize;

use crate::computer_use::ComputerAction;
use crate::risk_detector::RiskLevel;

use super::{ErrorCode, OpError};

/// Longest text one `type` action may enter, in characters.
pub const MAX_TYPE_CHARS: usize = 2_000;
/// Scroll clicks per `scroll` action.
pub const SCROLL_AMOUNT_RANGE: std::ops::RangeInclusive<i64> = 1..=20;
/// Default scroll clicks when the caller gives none.
pub const DEFAULT_SCROLL_AMOUNT: i64 = 3;

/// One action as the MCP client sends it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionRequest {
    Click {
        x: i64,
        y: i64,
        #[serde(default)]
        button: Option<String>,
        #[serde(default)]
        double: Option<bool>,
    },
    Type {
        text: String,
    },
    Key {
        key: String,
    },
    Scroll {
        x: i64,
        y: i64,
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        amount: Option<i64>,
    },
    /// Open a page (design §7.4). Validated against the session's pinned
    /// hosts by [`super::navigation::validate_url`], not by [`to_action`].
    Navigate {
        url: String,
    },
}

fn invalid(message: String) -> OpError {
    OpError::new(ErrorCode::InvalidAction, message)
}

fn coordinate(x: i64, y: i64, width: u32, height: u32) -> Result<[u32; 2], OpError> {
    let inside = x >= 0 && y >= 0 && x < i64::from(width) && y < i64::from(height);
    if !inside {
        return Err(invalid(format!(
            "座標 ({x}, {y}) 超出螢幕範圍：寬 {width}、高 {height}，座標從 0 起算。"
        )));
    }
    Ok([x as u32, y as u32])
}

/// Validate a request against the display and turn it into the action the
/// orchestrator executes. An invalid key is refused (never replaced by
/// `Escape`, the argv builder's last-resort fallback).
pub fn to_action(req: &ActionRequest, width: u32, height: u32) -> Result<ComputerAction, OpError> {
    match req {
        ActionRequest::Navigate { .. } => Err(invalid("導覽不是畫面操作，請改用 computer_navigate。".to_string())),
        ActionRequest::Click { x, y, button, double } => {
            let coordinate = coordinate(*x, *y, width, height)?;
            let double = double.unwrap_or(false);
            match button.as_deref().unwrap_or("left") {
                "left" if double => Ok(ComputerAction::DoubleClick { coordinate }),
                "left" => Ok(ComputerAction::LeftClick { coordinate }),
                "right" if double => Err(invalid("右鍵不支援連點兩下。".to_string())),
                "right" => Ok(ComputerAction::RightClick { coordinate }),
                _ => Err(invalid("滑鼠按鍵只接受 left 或 right。".to_string())),
            }
        }
        ActionRequest::Type { text } => {
            let chars = text.chars().count();
            if chars == 0 {
                return Err(invalid("要輸入的文字是空的。".to_string()));
            }
            if chars > MAX_TYPE_CHARS {
                return Err(invalid(format!(
                    "要輸入的文字有 {chars} 個字元，一次最多 {MAX_TYPE_CHARS} 個，請分段輸入。"
                )));
            }
            Ok(ComputerAction::Type { text: text.clone() })
        }
        ActionRequest::Key { key } => {
            let key = key.trim();
            if !crate::computer_use_orchestrator::validate_xdotool_key(key) {
                return Err(invalid(format!(
                    "按鍵「{}」不合法：只接受英數字與 + - _ 組成的按鍵或組合鍵，例如 ctrl+s、Return、Tab。",
                    duduclaw_core::truncate_chars(key, 64)
                )));
            }
            Ok(ComputerAction::Key { text: key.to_string() })
        }
        ActionRequest::Scroll { x, y, direction, amount } => {
            let coordinate = coordinate(*x, *y, width, height)?;
            let direction = match direction.as_deref().unwrap_or("down") {
                d @ ("up" | "down") => d.to_string(),
                _ => return Err(invalid("捲動方向只接受 up 或 down。".to_string())),
            };
            let amount = amount.unwrap_or(DEFAULT_SCROLL_AMOUNT);
            if !SCROLL_AMOUNT_RANGE.contains(&amount) {
                return Err(invalid(format!(
                    "捲動次數必須介於 {} 到 {}。",
                    SCROLL_AMOUNT_RANGE.start(),
                    SCROLL_AMOUNT_RANGE.end()
                )));
            }
            Ok(ComputerAction::Scroll { coordinate, direction, amount: amount as u32 })
        }
    }
}

/// What the risk step decides for one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Run it.
    Execute,
    /// Ask a human through the session's reply channel first.
    Confirm,
    /// Do not run it.
    Refuse(OpError),
}

/// Design §3.4 step 3. `contract_hit` is a CONTRACT.toml `must_not` match;
/// `has_confirmer` is whether the request's turn id names a live turn of
/// this employee whose reply channel resolved to a real sender.
pub fn risk_gate(
    risk: RiskLevel,
    contract_hit: bool,
    auto_confirm_trusted: bool,
    has_confirmer: bool,
) -> Gate {
    if contract_hit {
        return Gate::Refuse(OpError::new(
            ErrorCode::Blocked,
            "這個操作違反此員工 CONTRACT.toml 的 must_not 規則，已拒絕執行。",
        ));
    }
    match risk {
        RiskLevel::Blocked => Gate::Refuse(OpError::new(
            ErrorCode::Blocked,
            "這個操作被電腦操作安全規則擋下（焦點視窗屬於封鎖清單），已拒絕執行。",
        )),
        RiskLevel::High if auto_confirm_trusted => Gate::Execute,
        RiskLevel::High if has_confirmer => Gate::Confirm,
        RiskLevel::High => Gate::Refuse(OpError::new(
            ErrorCode::ConfirmationRequired,
            "這是高風險操作，需要有人在對話裡確認；這次呼叫沒有可詢問的對話通道（例如排程或委派），已拒絕執行。",
        )),
        RiskLevel::Medium | RiskLevel::Low => Gate::Execute,
    }
}

/// A short, human-readable line describing the action for the result text
/// and the audit. Typed text is never repeated, only its length.
pub fn describe(action: &ComputerAction) -> String {
    match action {
        ComputerAction::LeftClick { coordinate: [x, y] } => format!("左鍵點擊 ({x}, {y})"),
        ComputerAction::DoubleClick { coordinate: [x, y] } => format!("左鍵連點兩下 ({x}, {y})"),
        ComputerAction::RightClick { coordinate: [x, y] } => format!("右鍵點擊 ({x}, {y})"),
        ComputerAction::Type { text } => format!("輸入文字（{} 個字元）", text.chars().count()),
        ComputerAction::Key { text } => format!("按鍵 {text}"),
        ComputerAction::Scroll { coordinate: [x, y], direction, amount } => {
            let dir = if direction == "up" { "向上" } else { "向下" };
            format!("在 ({x}, {y}) {dir}捲動 {amount} 次")
        }
        other => format!("{other:?}"),
    }
}

/// Longest focused-window title shown in a confirmation prompt, in characters.
pub const PROMPT_TITLE_MAX_CHARS: usize = 60;

/// The focused window's title as it may appear in a confirmation prompt:
/// control characters (newlines included) removed, cut to
/// [`PROMPT_TITLE_MAX_CHARS`] characters. The title is set by the page, so
/// it is page-controlled text and is quoted as such by the prompt.
pub fn sanitize_window_title(title: &str) -> String {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    let cut = duduclaw_core::truncate_chars(clean, PROMPT_TITLE_MAX_CHARS);
    if clean.chars().count() > PROMPT_TITLE_MAX_CHARS {
        format!("{cut}…")
    } else {
        cut.to_string()
    }
}

/// The confirmation prompt shown to the human. It carries no
/// agent-controlled free text: typed text appears only as its character
/// count ([`describe`]), and the focused window's title is sanitised
/// ([`sanitize_window_title`]) and quoted as coming from the page.
pub fn confirmation_prompt(agent_id: &str, action: &ComputerAction, window: &str) -> String {
    let line = describe(action);
    let window = sanitize_window_title(window);
    format!(
        "⚠️ AI 員工 {agent_id} 要在電腦操作容器裡執行高風險操作：{line}\n焦點視窗標題（由網頁提供，僅供參考）：「{window}」\n要繼續嗎？（60 秒內未回覆視為拒絕）"
    )
}
