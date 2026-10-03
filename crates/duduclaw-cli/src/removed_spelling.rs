//! Retired CLI spellings (v1.69.0).
//!
//! A removed spelling is kept as a hidden stub that swallows whatever flags
//! the old command took, prints one sentence naming its replacement and exits
//! with status 2 without doing anything. Scripts that still use the old name
//! get an actionable message instead of clap's generic "unrecognized
//! subcommand". Every message lives in [`RemovedSpelling::message`], the one
//! table to edit when a spelling is retired.

/// Trailing arguments of a removed spelling. They are accepted so that the
/// old invocation parses whatever its flags were, and are never read.
#[derive(clap::Args, Debug)]
pub(crate) struct RemovedArgs {
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        num_args = 0..,
        hide = true
    )]
    _rest: Vec<String>,
}

/// The retired spellings; each maps to a replacement in [`Self::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemovedSpelling {
    MigrateFrom,
    Audit,
    GdprExport,
    PlaybookExport,
    AcpServer,
    ExpertInstall,
}

impl RemovedSpelling {
    /// User-facing sentence (zh-TW, CLI messages follow the UI language).
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::MigrateFrom => {
                "`duduclaw migrate-from` 已於 v1.69.0 移除，請改用 `duduclaw migrate from <平台>`。"
            }
            Self::Audit => {
                "`duduclaw audit` 已於 v1.69.0 移除，請改用 `duduclaw export audit`。"
            }
            Self::GdprExport => {
                "`duduclaw gdpr export` 已於 v1.69.0 移除，請改用 `duduclaw export gdpr <聯絡人>`。"
            }
            Self::PlaybookExport => {
                "`duduclaw playbook export` 已於 v1.69.0 移除，請改用 `duduclaw export playbook --agent <員工>`。"
            }
            Self::AcpServer => {
                "`duduclaw acp-server` 已於 v1.69.0 移除，請改用 `duduclaw acp server`。"
            }
            Self::ExpertInstall => {
                "`duduclaw expert install` 已於 v1.69.0 移除，請改用 `duduclaw pack install <來源>`。"
            }
        }
    }

    /// Print the message to stderr and exit with status 2 (usage error).
    pub(crate) fn exit(self) -> ! {
        eprintln!("{}", self.message());
        std::process::exit(2);
    }
}
