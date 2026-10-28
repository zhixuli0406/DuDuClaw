//! Complete native-only action details. This data must never enter an audit
//! log, a dashboard notification, or a model response.
use serde_json::Value;
pub(crate) fn render(action: &Value, element: Option<&str>) -> Result<String, String> {
    let raw=serde_json::to_string_pretty(&serde_json::json!({"requested_operation":action,"page_supplied_target_label_untrusted":element})).map_err(|_|"native action details unavailable")?;
    let mut rendered = String::new();
    for character in raw.chars() {
        if matches!(character,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{200e}'|'\u{200f}'|'\u{061c}')
        {
            use std::fmt::Write;
            write!(rendered, "\\u{:04x}", character as u32)
                .map_err(|_| "native action details unavailable")?;
        } else {
            rendered.push(character);
        }
        if rendered.len() > 64 * 1024 {
            return Err("native_action_details_too_large".into());
        }
    }
    Ok(rendered)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn native_details_include_the_complete_input_and_the_click_target() {
        let text = format!("{}END_OF_EXACT_INPUT", "字\n".repeat(2000));
        let rendered = render(
            &json!({"kind":"type","action":{"action":"type","text":text}}),
            None,
        )
        .unwrap();
        assert!(rendered.contains("END_OF_EXACT_INPUT"));
        let clicked = render(
            &json!({"kind":"click","action":{"action":"left_click","coordinate":[512,288]}}),
            Some("Synthetic submit button"),
        )
        .unwrap();
        assert!(
            clicked.contains("512")
                && clicked.contains("288")
                && clicked.contains("Synthetic submit button")
        );
    }
    #[test]
    fn native_details_never_silently_truncate_or_render_direction_controls_as_layout() {
        let rendered = render(
            &json!({"kind":"type","action":{"text":"prefix\u{202e}suffix"}}),
            None,
        )
        .unwrap();
        assert!(!rendered.contains('\u{202e}'));
        assert!(rendered.contains("\\u202e"));
        assert!(
            render(
                &json!({"kind":"type","action":{"text":"x".repeat(70000)}}),
                None
            )
            .is_err()
        );
    }
}
