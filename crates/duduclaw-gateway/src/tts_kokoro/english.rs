// Ported from hexgrad/misaki zh.py English-segment handling and us_gold lookup.
// Copyright hexgrad (misaki contributors). Licensed under Apache-2.0.
// https://www.apache.org/licenses/LICENSE-2.0
// Modified: Rust, lazy JSON dictionary, deterministic spelling fallback.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

static LEXICON: OnceLock<HashMap<String, String>> = OnceLock::new();

fn load(model_dir: &Path) -> Result<&'static HashMap<String, String>, String> {
    if let Some(lexicon) = LEXICON.get() { return Ok(lexicon); }
    let bytes = std::fs::read(model_dir.join("us_gold.json"))
        .map_err(|e| format!("Kokoro English lexicon: {e}"))?;
    let lexicon = parse(&bytes)?;
    let _ = LEXICON.set(lexicon);
    LEXICON.get().ok_or_else(|| "Kokoro English lexicon initialization failed".into())
}

fn parse(bytes: &[u8]) -> Result<HashMap<String,String>,String> {
    let raw: HashMap<String, serde_json::Value> = serde_json::from_slice(bytes)
        .map_err(|e| format!("Kokoro English lexicon JSON: {e}"))?;
    let mut lexicon = HashMap::new();
    for (key,value) in raw {
        let Some(phones) = value.as_str().or_else(|| value.get("DEFAULT").and_then(|d| d.as_str())) else { continue; };
        let lower = key.to_lowercase();
        if key == lower { lexicon.insert(lower, phones.to_owned()); }
        else { lexicon.entry(lower).or_insert_with(|| phones.to_owned()); }
    }
    Ok(lexicon)
}

pub(super) fn lookup(text: &str, lexicon: &HashMap<String, String>) -> String {
    text.split(|c: char| c.is_whitespace() || c == '-').filter(|s| !s.is_empty())
        .map(|word| {
            let word = word.to_lowercase();
            lexicon.get(&word).cloned().unwrap_or_else(|| {
                word.chars().filter_map(|c| lexicon.get(&c.to_string()).cloned())
                    .collect::<Vec<_>>().join(" ")
            })
        }).collect::<Vec<_>>().join(" ")
}

pub(super) fn phonemes(text: &str, model_dir: &Path) -> Result<String, String> {
    Ok(lookup(text, load(model_dir)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn string_and_pos_default_values() {
        let lexicon = parse(br#"{"hello":"h", "read":{"DEFAULT":"r", "VERB":"v"}}"#).unwrap();
        assert_eq!(lookup("Hello read", &lexicon), "h r");
    }
    #[test] fn unknown_is_spelled_and_hyphens_split() {
        let lexicon = HashMap::from([("a".into(), "ˈA".into()), ("b".into(), "bˈi".into())]);
        assert_eq!(lookup("ab-a", &lexicon), "ˈA bˈi ˈA");
    }
}
