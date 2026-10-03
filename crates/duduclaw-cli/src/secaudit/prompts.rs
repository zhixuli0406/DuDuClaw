//! Prompt text shared by the deep-audit and verifier prompts
//! (DESIGN-llm-contract-secaudit-v2 §3.4 items 2 and 4).
//!
//! Both prompts embed [`duduclaw_core::llm_contract::severity::ANCHORS_ZH_TW`]
//! and [`ANTI_PATTERNS`]: the auditor and the verifier are calibrated against
//! the same definitions, and the known model tendencies are listed as a
//! negative checklist in every prompt rather than left to "be careful".
//! The anti-pattern list is adapted from Cloudflare's security-audit-skill
//! `SKILL.md` (MIT), see `research/harness-2026-08/cloudflare-security-audit-skill-2026-10.md`.

pub use duduclaw_core::llm_contract::severity::ANCHORS_ZH_TW;

/// Ten things that are NOT a finding by themselves.
pub const ANTI_PATTERNS: &str = "\
## 反模式（以下情況本身不構成漏洞，不得回報）

1. 只是偏離檢查清單或「最佳實務」，卻說不出哪一道安全控制被實際擊破。
2. 縱深防禦少了一層（例如缺少額外的輸入檢查），但外層控制仍然完整擋住。
3. 猜測部署環境的行為（反向代理、防火牆、環境變數的實際值），卻沒有原始碼依據。
4. 同一個 principal 只能傷害自己（自己的資料、自己的工作階段），沒有跨越任何信任邊界。
5. 把當機、panic 或資源耗盡誇大成遠端程式碼執行或資料外洩。
6. 只用散文描述結果，沒有可以對照原始碼的檔案、行號與呼叫路徑。
7. 拿先前報告或其他工具的「已確認」當結論的依據，而不是重新從原始碼推導。
8. 需要攻擊者先取得一個本身就等同完全控制的權限（例如已能寫入設定檔或已有 root）。
9. 測試碼、範例碼、死碼或從未被呼叫的函式。
10. 已經被同一條路徑上的驗證、跳脫或權限檢查處理掉的輸入。
";

/// The six threat-model slots every candidate must fill (§3.2).
pub const THREAT_MODEL_INSTRUCTIONS: &str = "\
## 威脅模型（六格皆必填，每格都要有具體內容）

- principal：誰在行動（例如：未登入的網路使用者、已登入的一般使用者、另一個租戶）。
- input：此人實際能控制的輸入是什麼。
- control：本來應該擋住此人的安全控制是哪一個。
- boundary：被跨越的信任邊界是哪一條。
- affected：受影響的是誰的資料或哪一項資源。
- result：如果主張成立，具體會發生什麼後果。

填不出某一格，代表這不是一個可回報的發現。
";

/// Format of `trace` and `conditions` (§3.2).
pub const TRACE_CONDITIONS_INSTRUCTIONS: &str = "\
## trace 與 conditions 格式

trace 是從輸入到危險操作的步驟陣列，每步：
{\"kind\": \"entrypoint\"|\"propagation\"|\"sink\", \"file\": \"<repo 相對路徑>\", \"line\": <從 1 起算的行號>, \"scope\": \"<所在函式或方法名>\", \"description\": \"<這一步發生什麼>\"}
多步時第一步必須是 entrypoint、最後一步必須是 sink、中間全部是 propagation；只有一步時可為 entrypoint 或 sink。
每一步的 file 與 line 都必須真實存在，會被程式逐一檢查，不存在就整筆作廢。

conditions 是成立前提的陣列，每項：{\"kind\": \"<下列九種之一>\", \"description\": \"<具體前提>\"}
kind 只能是：authentication_level、authorization_role、user_interaction、system_configuration、network_routing、environmental_dependency、data_state、timing_dependency、third_party_dependency。
";

/// The coordinate system: every file line in the prompt is shown as
/// `<line number> | <source text>` (`llm_util::number_lines`).
pub const LINE_NUMBER_INSTRUCTIONS: &str = "\
## 行號

下方每一行原始碼都以「<行號> | <原始碼>」的形式呈現，行號從 1 起算，就是該檔案中的真實行號。
你回報的 line，以及 trace 每一步的 line，都必須直接抄寫該行開頭顯示的行號，不可自行計數或估算。
行號與「|」本身不是原始碼的一部分。
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anti_patterns_has_ten_numbered_items() {
        for n in 1..=10 {
            let marker = format!("\n{n}. ");
            assert!(ANTI_PATTERNS.contains(&marker), "missing item {n}");
        }
        assert!(!ANTI_PATTERNS.contains("\n11. "));
    }

    #[test]
    fn threat_model_instructions_name_all_six_slots() {
        for slot in [
            "principal",
            "input",
            "control",
            "boundary",
            "affected",
            "result",
        ] {
            assert!(THREAT_MODEL_INSTRUCTIONS.contains(&format!("- {slot}：")));
        }
    }

    #[test]
    fn trace_instructions_list_all_nine_condition_kinds() {
        for kind in [
            "authentication_level",
            "authorization_role",
            "user_interaction",
            "system_configuration",
            "network_routing",
            "environmental_dependency",
            "data_state",
            "timing_dependency",
            "third_party_dependency",
        ] {
            assert!(TRACE_CONDITIONS_INSTRUCTIONS.contains(kind));
        }
    }
}
