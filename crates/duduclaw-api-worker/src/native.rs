//! Native CLI input protocol. No executable/config/credential path is accepted.
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request { pub protocol: String, pub model: String, pub prompt: String }
impl Request {
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.arguments()?;
        let bytes = serde_json::to_vec(self).map_err(|_| "invalid native input")?;
        if bytes.len() > crate::MAX_INPUT { return Err("native input too large"); }
        Ok(bytes)
    }
    pub fn arguments(&self) -> Result<Vec<String>, &'static str> {
        if self.model.is_empty() || self.model.trim() != self.model || self.model.starts_with('-')
            || self.model.len() > 256 || self.model.chars().any(char::is_control)
            || self.prompt.is_empty() || self.prompt.len() > 64 * 1024 || self.prompt.as_bytes().contains(&0) {
            return Err("invalid native input");
        }
        let mut args: Vec<String> = match self.protocol.as_str() {
            "codex-v1" => ["exec", "--json", "--skip-git-repo-check", "--dangerously-bypass-approvals-and-sandbox", "--model"]
                .into_iter().map(str::to_owned).collect(),
            "grok-v1" => vec!["--model".into()],
            _ => return Err("unknown native protocol"),
        };
        args.push(self.model.clone());
        if self.protocol == "grok-v1" {
            // Print mode's -p value is one argument, never parsed as flags.
            let prompt = if self.prompt.starts_with('-') { format!(" {}", self.prompt) } else { self.prompt.clone() };
            args.extend(["--permission-mode".into(), "bypassPermissions".into(), "-p".into(), prompt]);
        } else { args.extend(["--".into(), self.prompt.clone()]); }
        Ok(args)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_protocol_has_no_host_paths_or_argument_injection() {
        for protocol in ["codex-v1", "grok-v1"] {
            let request = Request { protocol: protocol.into(), model: "model".into(), prompt: "--help 繁體\n$HOME".into() };
            let args = request.arguments().unwrap();
            assert_eq!(args.last().unwrap().trim_start(), request.prompt);
            assert!(request.encode().is_ok());
            let mut bad = request; bad.prompt = "x".repeat(65537); assert!(bad.encode().is_err());
        }
        assert!(serde_json::from_str::<Request>(r#"{"protocol":"codex-v1","model":"x","prompt":"hi","command":"sh"}"#).is_err());
    }
}
