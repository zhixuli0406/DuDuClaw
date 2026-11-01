//! Shared credential-safe profile export for the CLI and dashboard.
use duduclaw_core::agent_profile::{AgentProfile, ExportArtifact, ExportFormat, ProfileError};

pub fn export(profile: &AgentProfile, format: ExportFormat, include_user: bool) -> Result<Vec<ExportArtifact>, ProfileError> {
    duduclaw_core::agent_profile::export_with_credential_scan(profile, format, include_user, |text| {
        crate::credential_detect::mask_document(text).map(|masked| masked != text).map_err(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_export_format_refuses_context_credentials_without_values() {
        for format in [ExportFormat::Duduclaw, ExportFormat::Openclaw, ExportFormat::Hermes, ExportFormat::AgentsMd, ExportFormat::ClaudeSubagent] {
            for section in ["Persona", "Identity", "Rules", "Contract", "User", "Settings"] {
                let mut profile = AgentProfile { id: "worker".into(), soul: "Helpful assistant".into(), ..Default::default() };
                let secret = "db password: hunter2xyz";
                match section {
                    "Persona" => profile.soul = secret.into(),
                    "Identity" => profile.identity = secret.into(),
                    "Rules" => profile.rules = secret.into(),
                    "Contract" => profile.contract = format!("[boundaries]\nmust_not = ['{secret}']\n"),
                    "User" => profile.user = secret.into(),
                    _ => profile.description = secret.into(),
                }
                let error = export(&profile, format, true).unwrap_err().to_string();
                assert!(error.contains(section), "{format:?}: {error}");
                assert!(!error.contains("hunter2xyz"), "{format:?}: {error}");
            }
            let profile = AgentProfile { id: "worker".into(), soul: "Helpful assistant".into(), user: "db password: hunter2xyz".into(), ..Default::default() };
            let files = export(&profile, format, false).unwrap();
            assert!(!files.iter().any(|file| file.content.contains("hunter2xyz")));
        }
    }
}
