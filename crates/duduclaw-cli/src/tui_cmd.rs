//! Read-only operator control room. The gateway remains the authority.
use duduclaw_core::error::{DuDuClawError as Error, Result};
use duduclaw_tui::{
    ControlRoomOptions, InboxOptions, WatchOptions,
    auth::{AuthClient, LoginMethod, LoginMode, LoginPrompt, MfaMethod, PromptField, disable_core_dumps},
    client::{Client, ClientOptions},
    run_control_room, run_inbox, run_watch,
    transport::GatewayUrl,
};
use std::path::Path;
use zeroize::Zeroizing;

#[derive(clap::Subcommand, Clone, Debug)]
pub enum TuiCommands {
    /// Follow one employee without an alternate screen.
    #[command(about = duduclaw_tui::strings::WATCH_HELP)]
    Watch {
        agent: String,
        #[arg(long, help = duduclaw_tui::strings::WATCH_CONTENT_HELP)]
        content: bool,
    },
    /// 列出待處理事項；--follow 持續更新，--json 輸出 JSON Lines
    Inbox {
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        json: bool,
    },
}

// This Bash-lane matcher is a speed bump, not OS-level containment.
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw tui",
        path: &["tui"],
    }];

pub async fn run(home: &Path, command: Option<TuiCommands>) -> Result<()> {
    run_with_markers(home, command, &crate::ai_session_guard::markers()).await
}
async fn run_with_markers(
    home: &Path,
    command: Option<TuiCommands>,
    markers: &[&str],
) -> Result<()> {
    if !markers.is_empty() {
        return Err(Error::Config(
            duduclaw_tui::strings::AI_SESSION_REFUSED.into(),
        ));
    }
    // Must precede config, prompts, HTTP and any secret acquisition.
    disable_core_dumps().map_err(tui_error)?;
    let auth =
        AuthClient::new(GatewayUrl::for_home(home).map_err(tui_error)?).map_err(tui_error)?;
    let mut prompt = TerminalPrompt;
    // The gateway alone decides whether local auto-login is available (only
    // Personal). A uniform 403 leads to account login, without guessing edition.
    let mut mode = LoginMode::Personal;
    loop {
        let session = auth.login(mode, &mut prompt).await.map_err(tui_error)?;
        let client =
            Client::start(auth.clone(), session, ClientOptions::default()).map_err(tui_error)?;
        let result = match &command {
            None => {
                run_control_room(ControlRoomOptions {
                    client,
                    home: Some(home.to_owned()),
                })
                .await
            }
            Some(TuiCommands::Watch { agent, content }) => run_watch(WatchOptions { client, agent: agent.clone(), content: *content }).await,
            Some(TuiCommands::Inbox { follow, json }) => {
                run_inbox(InboxOptions {
                    client,
                    home: Some(home.to_owned()),
                    follow: *follow,
                    json: *json,
                })
                .await
            }
        };
        match result {
            Err(duduclaw_tui::Error::LoginRequired) => {
                eprintln!(
                    "{}",
                    duduclaw_tui::strings::error(duduclaw_tui::Error::LoginRequired)
                );
                // A failed refresh always requests explicit account login.
                // Never fall back to an automatic local session on re-login.
                mode = LoginMode::TeamOrEnterprise;
            }
            result => return result.map_err(tui_error),
        }
    }
}
fn tui_error(error: duduclaw_tui::Error) -> Error {
    // TUI errors contain only fixed text, never remote text or credentials.
    Error::Config(error.to_string())
}

struct TerminalPrompt;
#[async_trait::async_trait]
impl LoginPrompt for TerminalPrompt {
    fn is_interactive(&self) -> bool {
        console::Term::stderr().is_term()
    }
    fn mfa_retry(&mut self) {
        eprintln!("{}", duduclaw_tui::strings::MFA_RETRY);
    }
    async fn mfa_method(&mut self) -> duduclaw_tui::Result<MfaMethod> {
        tokio::task::spawn_blocking(|| {
            let term = console::Term::stderr();
            if !term.is_term() {
                return Err(duduclaw_tui::Error::MfaTerminalRequired);
            }
            let choice = dialoguer::Select::new()
                .with_prompt(duduclaw_tui::strings::MFA_METHOD)
                .items(&[
                    duduclaw_tui::strings::MFA_AUTHENTICATOR,
                    duduclaw_tui::strings::MFA_RECOVERY,
                ])
                .default(0)
                .interact_on(&term)
                .map_err(|_| duduclaw_tui::Error::PromptCancelled)?;
            Ok(if choice == 0 { MfaMethod::Authenticator } else { MfaMethod::RecoveryCode })
        }).await.map_err(|_| duduclaw_tui::Error::PromptCancelled)?
    }
    async fn method(&mut self) -> duduclaw_tui::Result<LoginMethod> {
        tokio::task::spawn_blocking(|| {
            let term = console::Term::stderr();
            if !term.is_term() {
                return Err(duduclaw_tui::Error::Terminal);
            }
            let choice = dialoguer::Select::new()
                .with_prompt(duduclaw_tui::strings::LOGIN_METHOD)
                .items(&[
                    duduclaw_tui::strings::LOGIN_PASSWORD,
                    duduclaw_tui::strings::LOGIN_EMAIL_CODE,
                ])
                .default(0)
                .interact_on(&term)
                .map_err(|_| duduclaw_tui::Error::PromptCancelled)?;
            Ok(if choice == 0 {
                LoginMethod::Password
            } else {
                LoginMethod::EmailCode
            })
        })
        .await
        .map_err(|_| duduclaw_tui::Error::PromptCancelled)?
    }
    async fn read(&mut self, field: PromptField) -> duduclaw_tui::Result<Zeroizing<String>> {
        tokio::task::spawn_blocking(move || {
            let term = console::Term::stderr();
            if !term.is_term() {
                return Err(duduclaw_tui::Error::Terminal);
            }
            // dialoguer/console own their input buffers; zeroization of their
            // internal allocations is not guaranteed. Move the result directly.
            let value = if field.secret() {
                dialoguer::Password::new()
                    .with_prompt(field.label())
                    .interact_on(&term)
            } else {
                dialoguer::Input::<String>::new()
                    .with_prompt(field.label())
                    .interact_text_on(&term)
            }
            .map_err(|_| duduclaw_tui::Error::PromptCancelled)?;
            Ok(Zeroizing::new(value))
        })
        .await
        .map_err(|_| duduclaw_tui::Error::PromptCancelled)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn tui_refuses_every_ai_marker_before_home_or_login() {
        let home = tempfile::tempdir().unwrap();
        let missing = home.path().join("must-not-be-created");
        for name in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            let markers = crate::ai_session_guard::markers_with(&|key| key == *name);
            for command in [
                None,
                Some(TuiCommands::Watch { agent: "adu".into(), content: false }),
                Some(TuiCommands::Inbox {
                    follow: false,
                    json: true,
                }),
                Some(TuiCommands::Inbox {
                    follow: true,
                    json: false,
                }),
            ] {
                let error = run_with_markers(&missing, command, &markers)
                    .await
                    .unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains(duduclaw_tui::strings::AI_SESSION_REFUSED)
                );
                assert!(!missing.exists());
            }
        }
    }
}
