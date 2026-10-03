//! Severity levels, the "overall ≤ impact" cap, and calibration anchors.
//!
//! Adapted from Cloudflare security-audit-skill (MIT) SKILL.md, section
//! "Separate priority from certainty": overall severity may never exceed the
//! impact that was actually demonstrated, and the anchors give the model a
//! fixed yardstick so the same result is graded the same way every run.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Ordered severity: `Informational < Low < Medium < High < Critical`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Informational,
    Low,
    Medium,
    High,
    Critical,
}

impl Level {
    /// All levels in ascending order.
    pub const ALL: [Level; 5] = [
        Level::Informational,
        Level::Low,
        Level::Medium,
        Level::High,
        Level::Critical,
    ];

    /// The snake_case wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Informational => "informational",
            Level::Low => "low",
            Level::Medium => "medium",
            Level::High => "high",
            Level::Critical => "critical",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// True when `overall <= impact`.
pub fn overall_within_impact(overall: Level, impact: Level) -> bool {
    overall <= impact
}

/// Cap `overall` at the demonstrated `impact` (`min`).
pub fn cap_overall(overall: Level, impact: Level) -> Level {
    overall.min(impact)
}

/// Calibration anchors for prompts (zh-TW).
pub const ANCHORS_ZH_TW: &str = "\
## 嚴重度錨點

只有已確認的結果才給嚴重度。可能性與影響必須依據實際示範出的條件與結果；整體嚴重度不得高於已示範的影響。
「待驗證」代表一個有原始碼依據、但被擋住無法驗證的邊界假設，它沒有嚴重度，也不能當成低信心的已確認漏洞。

- critical：未經認證的行為者取得程式碼執行、整個資料庫存取，或接管任意帳號。
- high：行為者完整擊破一道明確的安全控制，且有實際後果，例如認證繞過、跨租戶讀寫、影響其他使用者的儲存型腳本執行、已認證的程式碼執行、未經認證即可遠端停掉共用服務。
- medium：真實的邊界違反，但波及範圍有限、前提少見，或後果只限於一小組資源。
- low：洩漏非機密的內部資訊，或要花大量持續努力才換得極小效果。
- informational：已確認但影響極小的觀察，主要用來當更大發現的前置條件。

high 與 medium 的分界：示範出的結果是否完整擊破一道明確控制，而且那個動作有實際後果？只是削弱它就是 medium。
說不出具體損害時，嚴重度就比你感覺的低。
";

/// Calibration anchors for prompts (English).
pub const ANCHORS_EN: &str = "\
## Severity anchors

Only confirmed results receive severity. Likelihood and impact must reflect the demonstrated conditions and result; overall severity cannot exceed demonstrated impact.
\"Needs validation\" means a specific source-grounded boundary hypothesis is blocked; it has no severity and must not be treated as a low-confidence confirmed vulnerability.

- critical: an unauthenticated actor gains code execution, full data-store access, or takeover of arbitrary accounts.
- high: an actor fully defeats an explicit security control with real consequences: authentication bypass, cross-tenant read or write, stored script execution affecting other users, authenticated code execution, or an unauthenticated remote stop of a shared service.
- medium: a real boundary violation with limited blast radius, uncommon preconditions, or consequences confined to a narrow resource set.
- low: disclosure of non-secret internals, or an effect requiring sustained effort for minimal gain.
- informational: a confirmed but minimal-impact observation, useful mainly as a prerequisite inside a larger finding.

The high/medium discriminator: does the demonstrated result fully defeat an explicit control for an action with real consequences, or only weaken it?
If you cannot state the concrete damage, the severity is lower than it feels.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        assert!(Level::Informational < Level::Low);
        assert!(Level::Low < Level::Medium);
        assert!(Level::Medium < Level::High);
        assert!(Level::High < Level::Critical);
        let mut v = vec![
            Level::Critical,
            Level::Low,
            Level::High,
            Level::Informational,
            Level::Medium,
        ];
        v.sort();
        assert_eq!(v, Level::ALL.to_vec());
    }

    #[test]
    fn cap_and_within() {
        for &o in &Level::ALL {
            for &i in &Level::ALL {
                let capped = cap_overall(o, i);
                assert!(overall_within_impact(capped, i));
                assert_eq!(overall_within_impact(o, i), o <= i);
                assert_eq!(capped, if o <= i { o } else { i });
            }
        }
        assert_eq!(cap_overall(Level::Critical, Level::Medium), Level::Medium);
        assert_eq!(cap_overall(Level::Low, Level::High), Level::Low);
        assert!(!overall_within_impact(Level::High, Level::Low));
    }

    #[test]
    fn serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&Level::Informational).unwrap(),
            "\"informational\""
        );
        let l: Level = serde_json::from_str("\"critical\"").unwrap();
        assert_eq!(l, Level::Critical);
        assert!(serde_json::from_str::<Level>("\"Critical\"").is_err());
        for &l in &Level::ALL {
            assert_eq!(serde_json::to_string(&l).unwrap(), format!("\"{l}\""));
        }
    }

    #[test]
    fn anchors_name_every_level() {
        for &l in &Level::ALL {
            assert!(
                ANCHORS_ZH_TW.contains(&format!("- {}：", l.as_str())),
                "zh {l}"
            );
            assert!(ANCHORS_EN.contains(&format!("- {}:", l.as_str())), "en {l}");
        }
    }
}
