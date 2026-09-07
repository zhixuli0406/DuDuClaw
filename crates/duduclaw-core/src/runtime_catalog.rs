//! **The** runtime catalog — one compile-time table describing every AI CLI
//! backend DuDuClaw knows how to detect, install, authenticate and drive.
//!
//! ## Why this file exists
//!
//! Before WP-B (`docs/todo/TODO-ai-runtimes-2026-09.md` §3) the same set of
//! runtimes was written out by hand in *six* places — `RuntimeType`,
//! `CliKind`, `handle_runtime_detect`, `runtime_install::SPECS`,
//! `runtime_models::discover_all`, `cli_auth::spec_for` — plus the free-form
//! vendor lists inside error strings. Adding a backend meant finding all six
//! and getting each one right; missing one produced a runtime that could be
//! configured but never detected (or installed but never logged into). This
//! table is now the single source of truth those call sites read.
//!
//! ## Security properties (unchanged, and they must stay unchanged)
//!
//! The install whitelist is still **compile-time**: [`CATALOG`] is a `const`
//! slice of `&'static str`s. Nothing in it is ever assembled from caller
//! input; [`spec_for`] matches an exact, length-gated, ASCII-only identifier
//! against the table and returns `None` for everything else. There is no
//! default arm that could resolve an unknown name to a command. See
//! `duduclaw-gateway/src/runtime_install.rs` for the rest of the model
//! (admin-gated RPC, audit trail, per-provider concurrency guard, timeout).
//!
//! ## Verification discipline
//!
//! Every headless flag in this table is either taken from the vendor's own
//! documentation (and cited in the entry's comment) or the entry carries
//! [`RuntimeSpec::verified`] `= false`. **A flag is never invented.** An
//! unverified entry still detects, installs and reports its credentials
//! correctly — only its headless spawn is a best-effort guess that a live CLI
//! must confirm. `verified: false` is surfaced to the dashboard so the UI can
//! say "尚未實機驗證" rather than pretending.

/// A short human-facing string in the three locales DuDuClaw ships.
///
/// Any localized string in this table is either absent entirely or present in
/// **all three** locales — `runtime_catalog::tests::every_locale_text_is_complete`
/// enforces it, so a half-translated note can never reach the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocaleText {
    pub en: &'static str,
    pub zh_tw: &'static str,
    pub ja_jp: &'static str,
}

impl LocaleText {
    /// Pick the string for a BCP-47-ish locale tag. Anything unrecognised —
    /// including an empty tag — falls back to English rather than guessing.
    pub fn get(&self, locale: &str) -> &'static str {
        let tag = locale.trim().to_ascii_lowercase().replace('_', "-");
        if tag.starts_with("zh") {
            self.zh_tw
        } else if tag.starts_with("ja") {
            self.ja_jp
        } else {
            self.en
        }
    }

    /// True when every locale carries a non-empty string.
    pub fn is_complete(&self) -> bool {
        !self.en.trim().is_empty()
            && !self.zh_tw.trim().is_empty()
            && !self.ja_jp.trim().is_empty()
    }
}

/// How a CLI gets onto the host.
///
/// Every variant's payload is a compile-time constant. `runtime_install`
/// turns these into an argv (npm) or a `bash -c <constant>` (posix script);
/// nothing here is ever concatenated with caller input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallChannel {
    /// `npm install -g <package>`, spawned argv-wise (no shell). Works on
    /// macOS / Linux / Windows and lands the binary somewhere
    /// [`crate::which_cli_in_home`] already probes.
    Npm { package: &'static str },
    /// Vendor's official POSIX install script: `curl -fsSL <url> | bash`,
    /// run as `bash -c "<constant>"`. Unix only.
    PosixScript { url: &'static str },
    /// Direct binary download. `url_template` may contain `{version}`,
    /// `{os}` and `{arch}` placeholders; the OS image's
    /// `duduclaw-ai-runtimes` recipe (WP-F) is what actually expands them —
    /// the gateway never auto-installs through this channel, it only reports
    /// the URL, because a bare download has no vendor-signed integrity story
    /// the gateway can check.
    Binary { url_template: &'static str },
    /// PyPI console-script tool, installed into an isolated environment
    /// (`uv tool install <package>` / `pipx install <package>`). Not
    /// auto-installed by the gateway: it needs a Python toolchain decision
    /// the appliance image makes, not the dashboard.
    PythonTool { package: &'static str },
    /// A known command this gateway deliberately does not run. `reason` is a
    /// stable machine code the dashboard maps to an explanation; `command` is
    /// shown for the user to paste into a terminal.
    Manual {
        command: &'static str,
        reason: &'static str,
    },
}

impl InstallChannel {
    /// The human-readable one-liner shown in the UI and copied to the
    /// clipboard, for every channel including the argv-spawned npm path.
    pub fn command_display(&self) -> String {
        match self {
            Self::Npm { package } => format!("npm install -g {package}"),
            Self::PosixScript { url } => format!("curl -fsSL {url} | bash"),
            Self::Binary { url_template } => {
                format!("download {url_template} and put it on PATH")
            }
            Self::PythonTool { package } => format!("uv tool install {package}"),
            Self::Manual { command, .. } => (*command).to_string(),
        }
    }

    /// Stable machine code for the channel, for telemetry / the dashboard.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Npm { .. } => "npm",
            Self::PosixScript { .. } => "posix_script",
            Self::Binary { .. } => "binary",
            Self::PythonTool { .. } => "python_tool",
            Self::Manual { .. } => "manual",
        }
    }
}

/// What a headless run writes to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Plain text; the whole (trimmed) stdout is the answer.
    Text,
    /// One JSON document. The final assistant text is extracted by
    /// `runtime/generic_cli.rs`'s shape-agnostic walker.
    Json,
    /// JSON Lines / stream-json: one JSON document per line, the answer being
    /// the last line that carries assistant text.
    Jsonl,
}

impl OutputFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }
}

/// Placeholder token in [`HeadlessSpec::args_template`] that is replaced with
/// the full prompt payload (system instructions + history + user message) as
/// **one** argv element. A template WITHOUT this token delivers the prompt on
/// stdin instead — see [`HeadlessSpec::prompt_via_stdin`].
pub const PROMPT_PLACEHOLDER: &str = "{prompt}";

/// How to drive a CLI for one non-interactive turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessSpec {
    /// Argv template. [`PROMPT_PLACEHOLDER`] is substituted with the prompt
    /// payload; every other element is passed through verbatim. Model
    /// selection is NOT in here — it comes from [`Self::model_flag`] so the
    /// flag can be omitted entirely when no model is configured.
    pub args_template: &'static [&'static str],
    pub output: OutputFormat,
    /// Model selection flag, or `None` when the CLI has no `--model`.
    ///
    /// A flag ending in `=` is the **joined** form (`--model=<id>`, one argv
    /// element); anything else is the **separate** form (`--model`, `<id>`,
    /// two argv elements). GitHub Copilot CLI documents the joined form, so
    /// this distinction is load-bearing, not cosmetic.
    pub model_flag: Option<&'static str>,
    /// Environment variable that selects the model, for CLIs that expose **no**
    /// model flag at all (Mistral Vibe: `VIBE_ACTIVE_MODEL`). Applied only
    /// when [`Self::model_flag`] is `None`, so a CLI with both never gets a
    /// contradictory pair. `None` on both ⇒ the agent's configured model
    /// cannot be honoured and the runtime says so instead of pretending.
    pub model_env: Option<&'static str>,
    /// Environment stamped onto every headless spawn of this CLI (colour /
    /// telemetry suppression, non-interactive hints). Never secrets — the API
    /// key comes from [`AuthSpec::api_key_env`], read from the gateway's own
    /// environment at spawn time.
    pub extra_env: &'static [(&'static str, &'static str)],
}

impl HeadlessSpec {
    /// True when the prompt is written to the child's stdin rather than
    /// passed as an argument (i.e. the template has no [`PROMPT_PLACEHOLDER`]).
    pub const fn prompt_via_stdin(&self) -> bool {
        let mut i = 0;
        while i < self.args_template.len() {
            if str_eq(self.args_template[i], PROMPT_PLACEHOLDER) {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The two argv elements (or one, for the joined form) that select
    /// `model`, or an empty vec when this CLI has no model flag / the caller
    /// has no model to set.
    pub fn model_args(&self, model: &str) -> Vec<String> {
        let model = model.trim();
        match self.model_flag {
            Some(flag) if !model.is_empty() => {
                if let Some(prefix) = flag.strip_suffix('=') {
                    vec![format!("{prefix}={model}")]
                } else {
                    vec![flag.to_string(), model.to_string()]
                }
            }
            _ => Vec::new(),
        }
    }

    /// The `(name, value)` env pair that selects `model`, for a CLI whose only
    /// model channel is an environment variable. Empty whenever
    /// [`Self::model_args`] already handles it, or there is nothing to set.
    pub fn model_env_pair(&self, model: &str) -> Option<(&'static str, String)> {
        let model = model.trim();
        if model.is_empty() || self.model_flag.is_some() {
            return None;
        }
        self.model_env.map(|name| (name, model.to_string()))
    }
}

/// `const`-callable `&str` equality (`==` on `&str` is not `const` on stable).
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// How a user signs a CLI in, when an API key is not being used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    /// No interactive login — API key only.
    None,
    /// The CLI prints a URL + user code and polls in the background. Safe to
    /// drive from a **remote** dashboard: the user approves in their own
    /// browser and nothing has to reach a localhost callback.
    DeviceCode { args: &'static [&'static str] },
    /// The CLI opens a browser and waits for a redirect to a `localhost:<port>`
    /// listener it owns. Only completes when the dashboard and the browser are
    /// on the same machine (self-host); a Cloud dashboard must fall back to an
    /// API key.
    BrowserOauth { args: &'static [&'static str] },
    /// A login subcommand whose transport is neither of the above (e.g. a
    /// paste-back token flow like `claude setup-token`). Remote-safe.
    CliLogin { args: &'static [&'static str] },
}

impl LoginMethod {
    /// Argv appended to the CLI binary to start the login flow.
    pub fn args(&self) -> &'static [&'static str] {
        match self {
            Self::None => &[],
            Self::DeviceCode { args }
            | Self::BrowserOauth { args }
            | Self::CliLogin { args } => args,
        }
    }

    /// `true` ⇒ the flow completes for a dashboard that is NOT on the user's
    /// machine. `false` ⇒ localhost-callback, self-host only.
    pub fn remote_safe(&self) -> bool {
        !matches!(self, Self::BrowserOauth { .. } | Self::None)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::DeviceCode { .. } => "device_code",
            Self::BrowserOauth { .. } => "browser_oauth",
            Self::CliLogin { .. } => "cli_login",
        }
    }
}

/// How a runtime is authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthSpec {
    /// Canonical API-key environment variable, forwarded to the headless
    /// spawn when the gateway's own environment has it. `None` ⇒ the CLI has
    /// no documented key variable and must be logged in.
    pub api_key_env: Option<&'static str>,
    pub login: LoginMethod,
    /// Credential store paths **relative to `$HOME`**, most authoritative
    /// first. Presence + mtime only are ever read; content never is.
    pub credential_paths: &'static [&'static str],
    /// Vendor terms-of-service caveat the dashboard must show BEFORE a
    /// subscription login (decision §1-1: consumer-subscription tokens used
    /// by third-party products are server-side blocked by some vendors and
    /// have gotten accounts suspended). `None` ⇒ nothing special to say.
    pub tos_note: Option<LocaleText>,
    /// Short zh-TW/en/ja instruction shown next to the login button.
    pub login_hint: Option<LocaleText>,
}

/// One runtime's complete description.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeSpec {
    /// Canonical lowercase id. Matches the `RuntimeType` serde value, the
    /// `agent.toml [runtime] provider` value, and the `runtime.detect` /
    /// `runtime.install` RPC key.
    pub id: &'static str,
    /// Vendor's product name, for UI. Not translated (product names aren't).
    pub display_name: &'static str,
    /// Primary binary name on PATH.
    pub binary: &'static str,
    /// Accepted alternative identifiers for [`spec_for`] / `RuntimeType::parse`
    /// (e.g. `agy` → antigravity). NOT binary names — see
    /// [`Self::binary_aliases`].
    pub aliases: &'static [&'static str],
    /// Additional binary names probed after [`Self::binary`], for CLIs shipped
    /// under more than one name.
    pub binary_aliases: &'static [&'static str],
    pub install: InstallChannel,
    pub headless: HeadlessSpec,
    pub auth: AuthSpec,
    /// Native MCP server support (the CLI can be handed a `duduclaw` MCP
    /// server definition and will call its tools).
    pub mcp: bool,
    /// Model-id prefixes this runtime serves, lowercase. Drives
    /// `infer_provider_for_model` / `model_matches_provider`. Empty ⇒ serves
    /// arbitrary models (openai_compat), which those functions treat as
    /// "never a confident mismatch".
    pub model_prefixes: &'static [&'static str],
    /// Static `(id, label)` model list used when live discovery fails or the
    /// CLI has no non-interactive listing.
    pub fallback_models: &'static [(&'static str, &'static str)],
    pub vendor_url: &'static str,
    /// `false` ⇒ at least one headless/login flag in this entry could not be
    /// confirmed against vendor documentation or a live binary. See the
    /// entry's comment for exactly which. Never silently `true`.
    pub verified: bool,
}

// ── The table ────────────────────────────────────────────────────────────
//
// ORDER MATTERS for the dashboard's runtime list (it renders in this order):
// the five backends DuDuClaw has shipped and live-tested first, then the
// 2026-09 additions, then the API-only `openai_compat` catch-all last.

/// Every runtime DuDuClaw knows. Exhaustive: an id not in here is unknown and
/// is rejected everywhere (detect / install / login / model inference).
pub const CATALOG: &[RuntimeSpec] = &[
    // ── Claude Code ──────────────────────────────────────────────────────
    RuntimeSpec {
        id: "claude",
        display_name: "Claude Code",
        binary: "claude",
        // No `claude-code` alias on purpose: that is the npm package's tail,
        // and `runtime_install`'s whitelist test pins that a package-looking
        // name must never resolve to an install command.
        aliases: &[],
        binary_aliases: &[],
        // The npm package is marked deprecated upstream in favour of the
        // native installer but still installs and runs (TODO §2). Kept as the
        // channel because it is the one this repo's own
        // `container/Dockerfile.server` ships and smoke-tests, and because it
        // is the only channel with a working Windows story (`claude.exe`
        // inside the package).
        install: InstallChannel::Npm {
            package: "@anthropic-ai/claude-code",
        },
        headless: HeadlessSpec {
            // Descriptive only: `runtime/claude.rs` delegates to the
            // account-rotated `claude_runner` path, which builds its own argv
            // (session resume, `--allowedTools`, bare mode, …). Recorded here
            // so the catalog stays a complete description of every backend.
            args_template: &["-p", "{prompt}", "--output-format", "stream-json"],
            output: OutputFormat::Jsonl,
            model_flag: Some("--model"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            api_key_env: Some("ANTHROPIC_API_KEY"),
            login: LoginMethod::CliLogin {
                args: &["setup-token"],
            },
            credential_paths: &[".claude/.credentials.json"],
            tos_note: Some(LocaleText {
                en: "Anthropic blocks consumer Claude Pro/Max subscription tokens used by third-party products server-side (since 2026-03); accounts have been suspended. The API key path is the supported one.",
                zh_tw: "Anthropic 自 2026-03 起在伺服器端封鎖第三方產品使用消費者 Claude Pro/Max 訂閱 token，已有帳號被停權。建議走 API key。",
                ja_jp: "Anthropic は 2026-03 以降、サードパーティ製品による消費者向け Claude Pro/Max のサブスクリプション トークン利用をサーバー側でブロックしています（アカウント停止の事例あり）。API キーの利用を推奨します。",
            }),
            login_hint: Some(LocaleText {
                en: "Approve at the opened URL, then paste the verification code below and press Enter.",
                zh_tw: "在開啟的網址完成授權後，把驗證碼貼回下方並按 Enter。",
                ja_jp: "開いた URL で承認したあと、確認コードを下に貼り付けて Enter を押してください。",
            }),
        },
        mcp: true,
        model_prefixes: &["claude"],
        fallback_models: &[
            ("claude-opus-4-6", "Claude Opus 4.6"),
            ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
            ("claude-haiku-4-5", "Claude Haiku 4.5"),
        ],
        vendor_url: "https://docs.anthropic.com/en/docs/claude-code",
        verified: true,
    },
    // ── OpenAI Codex ─────────────────────────────────────────────────────
    RuntimeSpec {
        id: "codex",
        display_name: "OpenAI Codex CLI",
        binary: "codex",
        aliases: &[],
        binary_aliases: &[],
        install: InstallChannel::Npm {
            package: "@openai/codex",
        },
        headless: HeadlessSpec {
            // Verified in `runtime/codex.rs` (live-tested): `codex exec --json
            // <prompt>`, model via `-m`.
            args_template: &["exec", "--json", "{prompt}"],
            output: OutputFormat::Jsonl,
            model_flag: Some("-m"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            api_key_env: Some("OPENAI_API_KEY"),
            login: LoginMethod::BrowserOauth { args: &["login"] },
            credential_paths: &[".codex/auth.json"],
            tos_note: Some(LocaleText {
                en: "OpenAI's policy on third-party products driving a ChatGPT subscription login is unclear (TODO 2026-09 §1-1). The API key path is the supported one.",
                zh_tw: "OpenAI 對「第三方產品驅動 ChatGPT 訂閱登入」的政策不明（TODO 2026-09 §1-1），建議走 API key。",
                ja_jp: "サードパーティ製品が ChatGPT サブスクリプションのログインを利用することについて、OpenAI の方針は不明です（TODO 2026-09 §1-1）。API キーの利用を推奨します。",
            }),
            login_hint: Some(LocaleText {
                en: "Complete the OpenAI sign-in in a browser on this machine (localhost callback). Use an API key when remote.",
                zh_tw: "於同機瀏覽器完成 OpenAI 登入（localhost 回呼）。遠端請改用 API key。",
                ja_jp: "同一マシンのブラウザで OpenAI にサインインしてください（localhost コールバック）。リモートの場合は API キーをご利用ください。",
            }),
        },
        mcp: true,
        model_prefixes: &["gpt-", "o1", "o3", "codex"],
        fallback_models: &[
            ("gpt-5.6-sol", "GPT-5.6 Sol"),
            ("gpt-5.6-terra", "GPT-5.6 Terra"),
            ("gpt-5.6-luna", "GPT-5.6 Luna"),
            ("gpt-5.5", "GPT-5.5"),
            ("gpt-5.4", "GPT-5.4"),
            ("gpt-5.4-mini", "GPT-5.4 mini"),
            ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
        ],
        vendor_url: "https://github.com/openai/codex",
        verified: true,
    },
    // ── Google Gemini CLI ────────────────────────────────────────────────
    RuntimeSpec {
        id: "gemini",
        display_name: "Gemini CLI",
        binary: "gemini",
        aliases: &[],
        binary_aliases: &[],
        install: InstallChannel::Npm {
            package: "@google/gemini-cli",
        },
        headless: HeadlessSpec {
            // Verified in `runtime/gemini.rs` (live-tested): `gemini -p
            // --output-format stream-json <prompt>`, model via `-m`.
            args_template: &["-p", "--output-format", "stream-json", "{prompt}"],
            output: OutputFormat::Jsonl,
            model_flag: Some("-m"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            api_key_env: Some("GEMINI_API_KEY"),
            login: LoginMethod::BrowserOauth {
                args: &["auth", "login"],
            },
            credential_paths: &[".gemini/oauth_creds.json"],
            tos_note: Some(LocaleText {
                en: "Google blocks consumer subscription tokens used by third-party products server-side (since 2026-03). The API key path is the supported one.",
                zh_tw: "Google 自 2026-03 起在伺服器端封鎖第三方產品使用消費者訂閱 token，建議走 API key。",
                ja_jp: "Google は 2026-03 以降、サードパーティ製品による消費者向けサブスクリプション トークンの利用をサーバー側でブロックしています。API キーの利用を推奨します。",
            }),
            login_hint: Some(LocaleText {
                en: "Complete the Google sign-in in a browser on this machine (localhost callback).",
                zh_tw: "於同機瀏覽器完成 Google 登入（localhost 回呼）。",
                ja_jp: "同一マシンのブラウザで Google にサインインしてください（localhost コールバック）。",
            }),
        },
        mcp: true,
        model_prefixes: &["gemini"],
        fallback_models: &[
            ("gemini-3-pro-preview", "Gemini 3 Pro"),
            ("gemini-3-flash-preview", "Gemini 3 Flash"),
            ("gemini-2.5-pro", "Gemini 2.5 Pro"),
            ("gemini-2.5-flash", "Gemini 2.5 Flash"),
        ],
        vendor_url: "https://github.com/google-gemini/gemini-cli",
        verified: true,
    },
    // ── Google Antigravity (`agy`) ───────────────────────────────────────
    RuntimeSpec {
        id: "antigravity",
        display_name: "Google Antigravity",
        binary: "agy",
        aliases: &["agy"],
        binary_aliases: &[],
        // The official installer drops `agy` in `$HOME/.local/bin`, the FIRST
        // candidate `which_cli_in_home` probes — so post-install re-detect
        // turns green with no PATH surgery.
        install: InstallChannel::PosixScript {
            url: "https://antigravity.google/cli/install.sh",
        },
        headless: HeadlessSpec {
            // Verified in `runtime/antigravity.rs` against `agy --help` v1.0.12:
            // `agy -p <prompt>`, model via `--model` taking the DISPLAY name.
            args_template: &["-p", "{prompt}"],
            output: OutputFormat::Text,
            model_flag: Some("--model"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            api_key_env: Some("ANTIGRAVITY_API_KEY"),
            login: LoginMethod::BrowserOauth { args: &["login"] },
            // `agy` has no documented single credential file; presence is
            // inferred from the CLI itself, so the credential-store card is
            // skipped for this runtime.
            credential_paths: &[],
            tos_note: Some(LocaleText {
                en: "Same Google consumer-subscription restriction as Gemini CLI. Use ANTIGRAVITY_API_KEY when remote.",
                zh_tw: "與 Gemini CLI 相同的 Google 消費者訂閱限制。遠端請改用 ANTIGRAVITY_API_KEY。",
                ja_jp: "Gemini CLI と同じ Google の消費者向けサブスクリプション制限が適用されます。リモートの場合は ANTIGRAVITY_API_KEY をご利用ください。",
            }),
            login_hint: Some(LocaleText {
                en: "Complete the Antigravity sign-in in a browser on this machine. Use ANTIGRAVITY_API_KEY when remote.",
                zh_tw: "於同機瀏覽器完成 Antigravity 登入。遠端請改用 ANTIGRAVITY_API_KEY。",
                ja_jp: "同一マシンのブラウザで Antigravity にサインインしてください。リモートの場合は ANTIGRAVITY_API_KEY をご利用ください。",
            }),
        },
        mcp: true,
        model_prefixes: &["gemini"],
        fallback_models: &[
            ("Gemini 3.1 Pro (High)", "Gemini 3.1 Pro (High)"),
            ("Gemini 3.5 Flash (Medium)", "Gemini 3.5 Flash (Medium)"),
        ],
        vendor_url: "https://antigravity.google/docs/cli",
        verified: true,
    },
    // ── xAI Grok Build ───────────────────────────────────────────────────
    RuntimeSpec {
        id: "grok",
        display_name: "Grok Build",
        binary: "grok",
        aliases: &["grok-cli"],
        // The unrelated third-party `superagent-ai/grok-cli` ships as
        // `grok-cli`; probed last so a user who has it is still discovered.
        binary_aliases: &["grok-cli"],
        // Manual on purpose (pre-existing decision, kept): xAI's installer
        // puts the binary at `$HOME/.grok/bin/grok`, which
        // `which_cli_in_home` does NOT probe — an auto-install would finish
        // "successfully" and the wizard would still say 未安裝.
        install: InstallChannel::Manual {
            command: "curl -fsSL https://x.ai/cli/install.sh | bash",
            reason: "install_target_not_on_probe_path",
        },
        headless: HeadlessSpec {
            // Verified against docs.x.ai (2026-07-13) and live grok 0.2.111:
            // `-p, --single <PROMPT>` value-consuming, `-m/--model`.
            args_template: &["-p", "{prompt}"],
            output: OutputFormat::Text,
            model_flag: Some("--model"),
            model_env: None,
            extra_env: &[
                // The per-agent `.grok/config.toml` this platform writes only
                // activates once the folder is trusted; the dir is
                // duduclaw-managed, so the gate is disabled for our spawns.
                ("GROK_FOLDER_TRUST", "0"),
            ],
        },
        auth: AuthSpec {
            // Verified: XAI_API_KEY, NOT GROK_API_KEY.
            api_key_env: Some("XAI_API_KEY"),
            login: LoginMethod::DeviceCode {
                args: &["login", "--device-code"],
            },
            credential_paths: &[".grok/auth.json"],
            tos_note: Some(LocaleText {
                en: "Driving a SuperGrok / X Premium+ subscription from a third-party product is at the account holder's risk; xAI has not published a third-party embedding policy.",
                zh_tw: "以第三方產品驅動 SuperGrok／X Premium+ 訂閱由帳號持有人自行承擔風險；xAI 未公布第三方嵌入政策。",
                ja_jp: "サードパーティ製品から SuperGrok / X Premium+ のサブスクリプションを利用する場合、リスクはアカウント所有者が負います。xAI はサードパーティ組み込みに関する方針を公表していません。",
            }),
            login_hint: Some(LocaleText {
                en: "Open the URL below and enter the code in your browser — the CLI detects completion itself, no code needs pasting back here.",
                zh_tw: "開啟下方網址，在瀏覽器輸入驗證碼完成授權即可——CLI 會自動偵測完成，不需要在這裡貼回代碼。",
                ja_jp: "下の URL を開き、ブラウザでコードを入力してください。CLI が完了を自動検知するため、ここにコードを貼り戻す必要はありません。",
            }),
        },
        mcp: true,
        model_prefixes: &["grok"],
        fallback_models: &[("grok-build-0.1", "Grok Build 0.1"), ("grok-4", "Grok 4")],
        vendor_url: "https://docs.x.ai/build/cli",
        verified: true,
    },
    // ── Alibaba Qwen Code ────────────────────────────────────────────────
    //
    // Verified 2026-09-05 against the official headless guide
    // (qwenlm.github.io/qwen-code-docs/en/users/features/headless/) and the
    // auth guide (…/users/configuration/auth/):
    //   * npm `@qwen-code/qwen-code`, bin `qwen`, Node >= 22.
    //   * `qwen -p "<prompt>"` (prompt is the flag's VALUE; stdin also works).
    //   * `--output-format text|json|stream-json`; `json` is ONE buffered JSON
    //     **array** of events whose terminal `{"type":"result","result":"…"}`
    //     object carries the answer — exactly what `generic_cli`'s array walker
    //     reads from the end.
    //   * `--model <id>` (space-separated).
    //   * `--yolo` is required for a headless run to execute tools at all; the
    //     in-CLI `/auth` dialog is explicitly unsupported headless.
    RuntimeSpec {
        id: "qwen",
        display_name: "Qwen Code",
        binary: "qwen",
        aliases: &["qwen-code"],
        binary_aliases: &[],
        install: InstallChannel::Npm {
            package: "@qwen-code/qwen-code",
        },
        headless: HeadlessSpec {
            args_template: &["-p", "{prompt}", "--yolo", "--output-format", "json"],
            output: OutputFormat::Json,
            model_flag: Some("--model"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            // Qwen also reads OPENAI_API_KEY/OPENAI_BASE_URL, but forwarding
            // the gateway's OpenAI key to a DashScope endpoint would be a
            // credential-crossing bug, so the canonical variable here is the
            // ModelStudio/DashScope one.
            api_key_env: Some("DASHSCOPE_API_KEY"),
            // No login SUBCOMMAND exists: authentication is the interactive
            // `/auth` dialog, which the vendor documents as unavailable in
            // headless mode. With the free OAuth tier gone, API key is the
            // only path — so this is `None`, not an invented `qwen login`.
            login: LoginMethod::None,
            credential_paths: &[".qwen/.env", ".qwen/oauth_creds.json"],
            tos_note: Some(LocaleText {
                en: "The Qwen OAuth free tier was discontinued on 2026-04-15 — cached tokens may still work briefly, but new requests are rejected. Use an Alibaba ModelStudio / DashScope API key.",
                zh_tw: "Qwen 免費 OAuth 方案已於 2026-04-15 停用——舊 token 可能短期還能用，新請求會被拒絕。請改用阿里雲 ModelStudio／DashScope API key。",
                ja_jp: "Qwen の無料 OAuth ティアは 2026-04-15 に終了しました。キャッシュ済みトークンは一時的に動作する場合がありますが、新規リクエストは拒否されます。Alibaba ModelStudio / DashScope の API キーをご利用ください。",
            }),
            login_hint: Some(LocaleText {
                en: "Qwen Code has no headless login. Set a ModelStudio / DashScope API key instead.",
                zh_tw: "Qwen Code 沒有可在背景執行的登入流程，請改設定 ModelStudio／DashScope API key。",
                ja_jp: "Qwen Code にはヘッドレス ログインがありません。ModelStudio / DashScope の API キーを設定してください。",
            }),
        },
        mcp: true,
        model_prefixes: &["qwen"],
        fallback_models: &[
            // `qwen3-coder-plus` is the id used in the vendor's own headless
            // example; `-flash` is its documented cheaper sibling.
            ("qwen3-coder-plus", "Qwen3 Coder Plus"),
            ("qwen3-coder-flash", "Qwen3 Coder Flash"),
        ],
        vendor_url: "https://github.com/QwenLM/qwen-code",
        verified: true,
    },
    // ── Moonshot AI Kimi Code ────────────────────────────────────────────
    //
    // Verified 2026-09-05 against the vendor command reference
    // (kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command.html), the
    // env-var doc and the published `prompt-render.ts` source:
    //   * npm `@moonshot-ai/kimi-code`, bin `kimi`, Node >= 22.19.
    //   * `kimi -p "<prompt>"` — ARG only, no stdin path in `run-prompt.ts`.
    //   * `--output-format text|stream-json` (there is NO `json` value);
    //     stream-json is JSONL of OpenAI-chat-shaped messages, so the answer is
    //     the LAST `{"role":"assistant","content":"…"}` line.
    //   * `-m <alias>` (space-separated); the value is a provider alias such as
    //     `kimi-code/kimi-for-coding`, not a raw model id.
    //   * `--prompt` CONFLICTS with `--yolo`/`--auto`/`--plan` (startup error):
    //     `-p` already runs under the `auto` permission policy, which is why
    //     this template deliberately carries no auto-approve flag.
    //   * NOT to be confused with the separate Python `MoonshotAI/kimi-cli`
    //     product, whose `--print` flag does not exist here.
    RuntimeSpec {
        id: "kimi",
        display_name: "Kimi Code",
        binary: "kimi",
        aliases: &["kimi-code"],
        binary_aliases: &[],
        install: InstallChannel::Npm {
            package: "@moonshot-ai/kimi-code",
        },
        headless: HeadlessSpec {
            args_template: &["-p", "{prompt}", "--output-format", "stream-json"],
            output: OutputFormat::Jsonl,
            model_flag: Some("-m"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            // Kimi Code deliberately does NOT read a shell API key (a plain
            // `KIMI_API_KEY` export is documented as having no effect); keys
            // belong in `config.toml`. The one shell-readable channel is the
            // `KIMI_MODEL_*` family, and it only synthesizes a provider when
            // `KIMI_MODEL_NAME` is set alongside this key.
            api_key_env: Some("KIMI_MODEL_API_KEY"),
            login: LoginMethod::DeviceCode { args: &["login"] },
            credential_paths: &[".kimi-code/credentials"],
            tos_note: None,
            login_hint: Some(LocaleText {
                en: "Open the printed URL and enter the code in your browser — the CLI polls and finishes on its own.",
                zh_tw: "開啟畫面上的網址，在瀏覽器輸入驗證碼即可——CLI 會自行輪詢完成，不需要貼回代碼。",
                ja_jp: "表示された URL を開き、ブラウザでコードを入力してください。CLI が自動でポーリングして完了します。",
            }),
        },
        mcp: true,
        model_prefixes: &["kimi"],
        fallback_models: &[("kimi-code/kimi-for-coding", "Kimi for Coding")],
        vendor_url: "https://github.com/MoonshotAI/kimi-code",
        verified: true,
    },
    // ── GitHub Copilot CLI ───────────────────────────────────────────────
    //
    // Verified 2026-09-05 against docs.github.com's programmatic reference and
    // "run CLI programmatically" pages:
    //   * npm `@github/copilot`, bin `copilot`.
    //   * `copilot -p "<prompt>"` (stdin also accepted, but ignored when `-p`
    //     is present, so the ARG form is unambiguous).
    //   * `-s` = "suppress stats and decoration, outputting only the agent's
    //     response". **There is no JSON output mode** — plain stdout under `-s`
    //     is the entire machine-readable contract (confirmed absent across
    //     three doc pages), which is why `output` is Text and not a guess.
    //   * `--model=MODEL` — the docs use the JOINED form; whether a bare-space
    //     `--model X` also parses is UNVERIFIED, so the joined form is what
    //     this entry emits.
    //   * `--no-ask-user` stops clarifying questions but NOT tool approvals;
    //     `--allow-all-tools` is what the vendor's own example pairs with it
    //     for an unattended run (see this module's capability note).
    //   * `--headless --stdio` (the old `@github/copilot-sdk` interface) was
    //     REMOVED with no deprecation period — do not reach for it.
    RuntimeSpec {
        id: "copilot",
        display_name: "GitHub Copilot CLI",
        binary: "copilot",
        aliases: &["github-copilot"],
        binary_aliases: &[],
        install: InstallChannel::Npm {
            package: "@github/copilot",
        },
        headless: HeadlessSpec {
            args_template: &["-p", "{prompt}", "-s", "--no-ask-user", "--allow-all-tools"],
            output: OutputFormat::Text,
            model_flag: Some("--model="),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            // Credential precedence is COPILOT_GITHUB_TOKEN → GH_TOKEN →
            // GITHUB_TOKEN → keychain OAuth → `gh auth token`. Only the first
            // is Copilot-specific, so forwarding a generic GH_TOKEN from the
            // gateway's env is deliberately NOT done here.
            api_key_env: Some("COPILOT_GITHUB_TOKEN"),
            login: LoginMethod::DeviceCode {
                args: &["login", "--device-code"],
            },
            // `config.json` is the auto-managed app state; it is also where the
            // OAuth token lands when the OS keychain is unavailable.
            credential_paths: &[".copilot/config.json"],
            tos_note: Some(LocaleText {
                en: "Usage bills against the signed-in account's Copilot subscription, and a fine-grained PAT needs the account-level \"Copilot Requests\" permission.",
                zh_tw: "用量會計入登入帳號的 Copilot 訂閱；若用 fine-grained PAT，需要帳號層級的「Copilot Requests」權限。",
                ja_jp: "利用量はサインイン中のアカウントの Copilot サブスクリプションに課金されます。fine-grained PAT を使う場合はアカウント レベルの「Copilot Requests」権限が必要です。",
            }),
            login_hint: Some(LocaleText {
                en: "Open github.com/login/device and enter the printed code.",
                zh_tw: "開啟 github.com/login/device 並輸入畫面上的驗證碼。",
                ja_jp: "github.com/login/device を開き、表示されたコードを入力してください。",
            }),
        },
        mcp: true,
        // Copilot serves other vendors' models (`--model=gpt-5.4`,
        // `--model=claude-haiku-4.5`), so claiming a family would make it a
        // confident mismatch for its own catalogue. Left empty on purpose.
        model_prefixes: &[],
        fallback_models: &[
            ("gpt-5.4", "GPT-5.4 (via Copilot)"),
            ("claude-haiku-4.5", "Claude Haiku 4.5 (via Copilot)"),
        ],
        vendor_url: "https://docs.github.com/en/copilot/concepts/agents/about-copilot-cli",
        verified: true,
    },
    // ── AWS Kiro CLI ─────────────────────────────────────────────────────
    //
    // Verified 2026-09-05 against kiro.dev/docs (installation, cli/headless,
    // reference/cli-commands, getting-started/authentication):
    //   * `curl -fsSL https://cli.kiro.dev/install | bash`, bin `kiro-cli`
    //     (the legacy Amazon Q `q` name still resolves). No npm, no Homebrew.
    //   * `kiro-cli chat --no-interactive --trust-all-tools "<prompt>"` —
    //     prompt is a POSITIONAL argument; `--trust-all-tools` is documented as
    //     required for an unattended run.
    //   * OUTPUT: `--output-format stream-json` exists but (a) additionally
    //     requires `--engine v2|v3` and (b) its event schema is undocumented
    //     (vendor issue still open). Parsing it would mean inventing a shape,
    //     so this entry uses the DOCUMENTED default instead: `--no-interactive`
    //     "prints the first response to STDOUT" as plain text.
    //   * There is NO `--model` flag. Model comes from
    //     `kiro-cli settings chat.defaultModel <id>` or an `--agent` profile —
    //     neither of which is a per-call channel, so both model fields are
    //     `None` and the runtime reports that rather than pretending.
    RuntimeSpec {
        id: "kiro",
        display_name: "Kiro CLI",
        binary: "kiro-cli",
        aliases: &["kiro-cli", "amazon-q"],
        binary_aliases: &[],
        // Not auto-run: an unattended install here would land a CLI whose own
        // terms forbid the way this platform would drive it (see `tos_note`).
        // The command is shown so an operator can make that call deliberately.
        install: InstallChannel::Manual {
            command: "curl -fsSL https://cli.kiro.dev/install | bash",
            reason: "vendor_tos_restricts_third_party_harness",
        },
        headless: HeadlessSpec {
            args_template: &["chat", "--no-interactive", "--trust-all-tools", "{prompt}"],
            output: OutputFormat::Text,
            model_flag: None,
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            // `ksk_…`, generated in the Kiro web console; paid plans only.
            // NOTE (verified): a browser session created by `kiro-cli login`
            // takes PRECEDENCE over this variable.
            api_key_env: Some("KIRO_API_KEY"),
            login: LoginMethod::DeviceCode {
                args: &["login", "--use-device-flow"],
            },
            // The settings paths are documented; where the token itself lands
            // is not, so only the directory that certainly exists after a
            // login is listed.
            credential_paths: &[".kiro/settings/cli.json"],
            tos_note: Some(LocaleText {
                en: "BLOCKING: Kiro's FAQ states that use through third-party automation harnesses that route requests outside Kiro's native interfaces is not permitted. Driving Kiro from DuDuClaw is exactly that. Calling kiro-cli directly from your own CI is allowed. Verify the current terms before enabling this runtime.",
                zh_tw: "阻礙事項：Kiro FAQ 明文寫著「不允許透過第三方自動化 harness、將請求繞過 Kiro 原生介面」。用 DuDuClaw 驅動 Kiro 正屬此類。自行在 CI 直接呼叫 kiro-cli 則被允許。啟用本 runtime 前請自行確認最新條款。",
                ja_jp: "重要：Kiro の FAQ には、Kiro のネイティブ インターフェース以外へリクエストを流すサードパーティ製自動化ハーネス経由の利用は許可されないと明記されています。DuDuClaw から Kiro を駆動することはこれに該当します。自身の CI から kiro-cli を直接呼び出すことは許可されています。本ランタイムを有効化する前に最新の規約をご確認ください。",
            }),
            login_hint: Some(LocaleText {
                en: "Open the printed URL and enter the code (GitHub / Google / Builder ID / IAM Identity Center).",
                zh_tw: "開啟畫面上的網址並輸入驗證碼（GitHub／Google／Builder ID／IAM Identity Center 擇一）。",
                ja_jp: "表示された URL を開き、コードを入力してください（GitHub / Google / Builder ID / IAM Identity Center）。",
            }),
        },
        mcp: true,
        model_prefixes: &[],
        fallback_models: &[],
        vendor_url: "https://kiro.dev/docs/cli/headless",
        verified: true,
    },
    // ── Cursor CLI ───────────────────────────────────────────────────────
    //
    // Verified 2026-09-05 against cursor.com/docs/cli (installation, headless,
    // reference/parameters, reference/authentication) plus a local
    // `cursor-agent --help`:
    //   * `curl https://cursor.com/install -fsS | bash`.
    //   * BINARY NAME: the installer creates BOTH `~/.local/bin/agent` and
    //     `~/.local/bin/cursor-agent`. `agent` COLLIDES with the xAI Grok
    //     installer's own `~/.local/bin/agent` (observed on this very machine:
    //     grok installed second and won). This entry therefore binds to
    //     `cursor-agent` only — probing `agent` could silently drive Grok.
    //   * `cursor-agent -p "<prompt>" --force` (`-f/--force` = "force allow
    //     commands unless explicitly denied"); prompt is POSITIONAL.
    //   * `--output-format text|json|stream-json` (only with `--print`); `json`
    //     puts the answer in `.result`.
    //   * `--model <id>` (space-separated).
    RuntimeSpec {
        id: "cursor",
        display_name: "Cursor CLI",
        binary: "cursor-agent",
        aliases: &["cursor-agent"],
        // Deliberately NOT `agent` — see the binary-name note above.
        binary_aliases: &[],
        install: InstallChannel::PosixScript {
            url: "https://cursor.com/install",
        },
        headless: HeadlessSpec {
            args_template: &["-p", "{prompt}", "--force", "--output-format", "json"],
            output: OutputFormat::Json,
            model_flag: Some("--model"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            api_key_env: Some("CURSOR_API_KEY"),
            // `cursor-agent login` opens a browser; `NO_OPEN_BROWSER=1` makes
            // it print the URL instead, which is what a headless host needs.
            login: LoginMethod::BrowserOauth { args: &["login"] },
            // UNVERIFIED path: the docs say only "securely stored locally".
            // `~/.cursor/cli-config.json` is what a logged-in host actually
            // has, so it is the presence probe — its CONTENT is never read.
            credential_paths: &[".cursor/cli-config.json"],
            tos_note: Some(LocaleText {
                en: "Cursor's terms carry no headless/automation restriction, and an API key is the vendor-recommended CI path. Reverse engineering and training competing models on its output are prohibited.",
                zh_tw: "Cursor 條款未限制 headless／自動化用途，API key 也是官方建議的 CI 路徑；但禁止逆向工程與用其輸出訓練競品模型。",
                ja_jp: "Cursor の規約にヘッドレス／自動化の制限はなく、API キーはベンダー推奨の CI 経路です。リバース エンジニアリングや出力を用いた競合モデルの学習は禁止されています。",
            }),
            login_hint: Some(LocaleText {
                en: "Complete the Cursor sign-in in a browser on this machine. Use CURSOR_API_KEY when remote.",
                zh_tw: "於同機瀏覽器完成 Cursor 登入。遠端請改用 CURSOR_API_KEY。",
                ja_jp: "同一マシンのブラウザで Cursor にサインインしてください。リモートの場合は CURSOR_API_KEY をご利用ください。",
            }),
        },
        mcp: true,
        // Cursor proxies several vendors' models (`gpt-5`, `sonnet-4`, …), so
        // no family claim — same reasoning as Copilot.
        model_prefixes: &[],
        fallback_models: &[
            ("sonnet-4", "Claude Sonnet 4 (via Cursor)"),
            ("sonnet-4-thinking", "Claude Sonnet 4 Thinking (via Cursor)"),
            ("gpt-5", "GPT-5 (via Cursor)"),
        ],
        vendor_url: "https://cursor.com/docs/cli/headless",
        verified: true,
    },
    // ── Mistral Vibe ─────────────────────────────────────────────────────
    //
    // Verified 2026-09-05 against the published `mistral-vibe` source
    // (`vibe/cli/entrypoint.py` argparse, `vibe/core/config/vibe_schema.py`)
    // and docs.mistral.ai/vibe/code/cli:
    //   * PyPI `mistral-vibe` (Python >= 3.12), console script `vibe`.
    //   * `vibe -p "<prompt>"` — the prompt is the FLAG's value. A bare
    //     positional (`vibe "text"`) opens the TUI instead, and none of
    //     `run` / `--headless` / `--print` / `--non-interactive` exist.
    //   * `--output text|json|streaming` (note: `--output`, not
    //     `--output-format`). `json` prints the whole history array; the answer
    //     is the last `type=="message" && role=="assistant"` element's
    //     concatenated text blocks — which is what `generic_cli`'s array walker
    //     reads, filtering non-assistant events.
    //   * There is NO `--model` flag; the model is `VIBE_ACTIVE_MODEL` (an
    //     alias from `[[models]]`, not an API model name).
    //   * `--yolo` alone is not enough: without `--trust`, an untrusted
    //     directory's project settings are silently ignored.
    RuntimeSpec {
        id: "vibe",
        display_name: "Mistral Vibe",
        binary: "vibe",
        aliases: &["mistral-vibe"],
        binary_aliases: &[],
        install: InstallChannel::PythonTool {
            package: "mistral-vibe",
        },
        headless: HeadlessSpec {
            args_template: &["-p", "{prompt}", "--yolo", "--trust", "--output", "json"],
            output: OutputFormat::Json,
            model_flag: None,
            model_env: Some("VIBE_ACTIVE_MODEL"),
            extra_env: &[
                // Telemetry defaults to ON; an appliance run should not phone
                // home on the operator's behalf.
                ("VIBE_ENABLE_TELEMETRY", "false"),
            ],
        },
        auth: AuthSpec {
            api_key_env: Some("MISTRAL_API_KEY"),
            // There is no `vibe login`; browser sign-in happens inside the
            // interactive `vibe --setup` wizard, which is not drivable
            // headlessly — so `None` rather than an invented subcommand.
            login: LoginMethod::None,
            credential_paths: &[".vibe/.env", ".vibe/config.toml"],
            tos_note: Some(LocaleText {
                en: "Apache-2.0 CLI; Mistral publishes a GitHub Action that runs `vibe -p`, so automation is accepted in practice. The written terms could not be retrieved — verify before commercial use.",
                zh_tw: "CLI 本身為 Apache-2.0；Mistral 官方 GitHub Action 就是跑 `vibe -p`，實務上接受自動化。條款正文無法擷取，商用前請自行確認。",
                ja_jp: "CLI 自体は Apache-2.0 で、Mistral 公式の GitHub Action が `vibe -p` を実行しているため、実務上は自動化が受け入れられています。規約本文は取得できなかったため、商用利用の前にご確認ください。",
            }),
            login_hint: Some(LocaleText {
                en: "Mistral Vibe has no headless login. Set MISTRAL_API_KEY instead.",
                zh_tw: "Mistral Vibe 沒有可在背景執行的登入流程，請改設定 MISTRAL_API_KEY。",
                ja_jp: "Mistral Vibe にはヘッドレス ログインがありません。MISTRAL_API_KEY を設定してください。",
            }),
        },
        mcp: true,
        model_prefixes: &["mistral", "codestral", "devstral", "magistral"],
        fallback_models: &[
            // These are Vibe's `[[models]]` ALIASES, which is what
            // VIBE_ACTIVE_MODEL takes — not raw API model names.
            ("mistral-medium-3.5", "Mistral Medium 3.5 (default alias)"),
            ("local", "Local devstral via llama.cpp"),
        ],
        vendor_url: "https://docs.mistral.ai/vibe/code/cli/work-with-cli",
        verified: true,
    },
    // ── OpenCode ─────────────────────────────────────────────────────────
    //
    // Verified 2026-09-05 against opencode.ai/docs plus the published
    // `packages/opencode/src/cli/cmd/run.ts`:
    //   * The repo moved `sst/opencode` → `anomalyco/opencode` (MIT). Install
    //     script `https://opencode.ai/install`; npm package `opencode-ai`.
    //   * `opencode run "<prompt>"` — positional AND stdin (concatenated when
    //     both are present).
    //   * OUTPUT FLAG IS `--format default|json`, **not** `--output-format`.
    //     `json` is NDJSON; the answer is in `type=="text"` events at
    //     `.part.text`.
    //   * `--model provider/model` (e.g. `anthropic/claude-sonnet-4-5`).
    //   * `--auto` is required: without it, `run` AUTO-DENIES anything that
    //     resolves to `ask` (silently), rather than prompting.
    //     (`--yolo` / `--dangerously-skip-permissions` are undocumented source
    //     aliases and are deliberately not used.)
    RuntimeSpec {
        id: "opencode",
        display_name: "OpenCode",
        binary: "opencode",
        aliases: &[],
        binary_aliases: &[],
        install: InstallChannel::PosixScript {
            url: "https://opencode.ai/install",
        },
        headless: HeadlessSpec {
            args_template: &["run", "{prompt}", "--auto", "--format", "json"],
            output: OutputFormat::Jsonl,
            model_flag: Some("--model"),
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            // OpenCode is multi-provider: it reads whichever provider key
            // models.dev names (ANTHROPIC_API_KEY, OPENAI_API_KEY, …). Only
            // its own key is forwarded from here; the rest belong to the
            // provider the agent actually selected and are not this runtime's
            // to guess.
            api_key_env: Some("OPENCODE_API_KEY"),
            login: LoginMethod::CliLogin {
                args: &["auth", "login"],
            },
            credential_paths: &[".local/share/opencode/auth.json"],
            tos_note: Some(LocaleText {
                en: "MIT, no usage restriction of its own. Provider terms still apply: OpenCode removed its Anthropic subscription plugin in 1.3.0 because driving a Pro/Max subscription from a third party is prohibited — use provider API keys.",
                zh_tw: "MIT 授權、本身無使用限制，但供應商條款仍適用：OpenCode 已在 1.3.0 移除 Anthropic 訂閱 plugin，因為第三方驅動 Pro/Max 訂閱屬禁止行為——請改用各供應商的 API key。",
                ja_jp: "MIT ライセンスで独自の利用制限はありませんが、プロバイダーの規約は適用されます。サードパーティから Pro/Max サブスクリプションを利用することは禁止されているため、OpenCode は 1.3.0 で Anthropic サブスクリプション プラグインを削除しました。プロバイダーの API キーをご利用ください。",
            }),
            login_hint: Some(LocaleText {
                en: "Pick a provider and paste its API key when prompted.",
                zh_tw: "選擇供應商後，依提示貼上該供應商的 API key。",
                ja_jp: "プロバイダーを選び、プロンプトに従って API キーを貼り付けてください。",
            }),
        },
        mcp: true,
        // Multi-provider shell: `--model` takes `provider/model`, so no family
        // claim — same reasoning as Copilot and Cursor.
        model_prefixes: &[],
        fallback_models: &[
            ("anthropic/claude-sonnet-4-5", "Claude Sonnet 4.5 (via OpenCode)"),
            ("openai/gpt-5", "GPT-5 (via OpenCode)"),
        ],
        vendor_url: "https://opencode.ai/docs/cli/",
        verified: true,
    },
    // ── OpenAI-compatible HTTP (no CLI) ──────────────────────────────────
    RuntimeSpec {
        id: "openai_compat",
        display_name: "OpenAI-compatible endpoint",
        // Not a CLI: `runtime/openai_compat.rs` speaks HTTP. The binary name
        // is never probed (see `which_runtime`, which short-circuits on an
        // empty binary), but the field is non-optional so every other call
        // site can stay uniform.
        binary: "",
        aliases: &["openai", "openai-compat"],
        binary_aliases: &[],
        install: InstallChannel::Manual {
            command: "(no CLI — configure an endpoint in inference.toml)",
            reason: "http_endpoint_not_a_cli",
        },
        headless: HeadlessSpec {
            args_template: &[],
            output: OutputFormat::Json,
            model_flag: None,
            model_env: None,
            extra_env: &[],
        },
        auth: AuthSpec {
            api_key_env: Some("OPENAI_API_KEY"),
            login: LoginMethod::None,
            credential_paths: &[],
            tos_note: None,
            login_hint: None,
        },
        mcp: false,
        // Empty on purpose: this backend proxies arbitrary models, so it must
        // never be a confident mismatch for any model id.
        model_prefixes: &[],
        fallback_models: &[],
        vendor_url: "https://platform.openai.com/docs/api-reference/chat",
        verified: true,
    },
];

// ── Lookup ───────────────────────────────────────────────────────────────

/// Longest legitimate runtime identifier, with headroom. A longer argument is
/// a probe, not a typo, and is rejected before it can cost an allocation.
const MAX_ID_LEN: usize = 32;

/// Resolve an identifier (canonical id or documented alias) to its spec.
///
/// Matching is exact after trimming + ASCII-lowercasing. Anything else — an
/// unknown name, a shell payload, a path, an npm package name, a homoglyph —
/// returns `None`. Deliberately **not** built on
/// `RuntimeType::parse`, whose unknown arm falls back to a default runtime:
/// here an unrecognised name must never resolve to a command.
pub fn spec_for(id: &str) -> Option<&'static RuntimeSpec> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_ID_LEN {
        return None;
    }
    let key = trimmed.to_ascii_lowercase();
    // Bare ASCII identifier only. Also rules out homoglyphs (Cyrillic `с`,
    // fullwidth `ａ`), which `to_ascii_lowercase` leaves untouched.
    if !key
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return None;
    }
    CATALOG
        .iter()
        .find(|s| s.id == key || s.aliases.contains(&key.as_str()))
}

/// Every runtime id, in catalog order.
pub fn all_ids() -> impl Iterator<Item = &'static str> {
    CATALOG.iter().map(|s| s.id)
}

/// `"claude|codex|gemini|…"` — the vendor list embedded in error strings, so
/// a new runtime never has to be remembered in a message somewhere.
pub fn id_list_pipe() -> String {
    CATALOG
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>()
        .join("|")
}

/// Runtimes that are driven as a local CLI (i.e. everything except the
/// HTTP-only `openai_compat`). These are the ones detection, install and
/// login apply to.
pub fn cli_specs() -> impl Iterator<Item = &'static RuntimeSpec> {
    CATALOG.iter().filter(|s| !s.binary.is_empty())
}

/// Best-effort model-family → runtime, by the longest matching prefix in
/// [`RuntimeSpec::model_prefixes`]. `None` for an unknown family — never a
/// guess.
///
/// Two tie-breaks, both load-bearing:
///   * **Longest prefix wins.** `codex` and `gpt-` both belong to the codex
///     entry, but an entry claiming a longer, more specific prefix must beat a
///     broad one.
///   * **On an equal-length tie, the FIRST catalog entry wins.** `gemini` and
///     `antigravity` both serve `gemini-*`; catalog order is preference order,
///     so `gemini-3-pro` resolves to the Gemini CLI, not to `agy`.
pub fn runtime_for_model(model: &str) -> Option<&'static RuntimeSpec> {
    let lower = model.trim().to_ascii_lowercase();
    // Accept the qualified `provider/model` form — take the model part.
    let m = lower.rsplit('/').next().unwrap_or(&lower);
    if m.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &'static RuntimeSpec)> = None;
    for spec in CATALOG {
        let Some(len) = spec
            .model_prefixes
            .iter()
            .filter(|p| m.starts_with(**p))
            .map(|p| p.len())
            .max()
        else {
            continue;
        };
        // Strictly greater — so an equal-length later entry never displaces an
        // earlier one.
        if best.is_none_or(|(best_len, _)| len > best_len) {
            best = Some((len, spec));
        }
    }
    best.map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_and_well_formed() {
        let mut seen = HashSet::new();
        for spec in CATALOG {
            assert!(seen.insert(spec.id), "duplicate runtime id `{}`", spec.id);
            assert!(!spec.id.is_empty());
            assert!(spec.id.len() <= MAX_ID_LEN, "id `{}` too long", spec.id);
            assert!(
                spec.id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "id `{}` must be lowercase ascii/_/digits — spec_for() rejects anything else",
                spec.id
            );
        }
    }

    #[test]
    fn binaries_are_unique() {
        let mut seen = HashSet::new();
        for spec in cli_specs() {
            assert!(
                seen.insert(spec.binary),
                "two runtimes claim binary `{}` — detection would be ambiguous",
                spec.binary
            );
        }
    }

    #[test]
    fn aliases_never_collide_with_an_id_or_another_alias() {
        let mut seen: HashSet<&str> = CATALOG.iter().map(|s| s.id).collect();
        for spec in CATALOG {
            for alias in spec.aliases {
                assert!(
                    seen.insert(alias),
                    "alias `{alias}` on `{}` collides with an id or another alias",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn spec_for_round_trips_ids_and_aliases() {
        for spec in CATALOG {
            assert_eq!(spec_for(spec.id).map(|s| s.id), Some(spec.id));
            assert_eq!(
                spec_for(&spec.id.to_ascii_uppercase()).map(|s| s.id),
                Some(spec.id),
                "lookup must be case-insensitive"
            );
            for alias in spec.aliases {
                assert_eq!(
                    spec_for(alias).map(|s| s.id),
                    Some(spec.id),
                    "alias `{alias}` must resolve to `{}`",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn spec_for_rejects_hostile_input() {
        for bad in [
            "",
            "   ",
            "claude; rm -rf /",
            "../../etc/passwd",
            "@anthropic-ai/claude-code",
            "сlaude", // Cyrillic 'с'
            "ｃlaude", // fullwidth
            "totally-unknown",
        ] {
            assert!(spec_for(bad).is_none(), "must reject {bad:?}");
        }
        assert!(spec_for(&"a".repeat(4096)).is_none(), "length gate");
    }

    #[test]
    fn every_locale_text_is_complete() {
        for spec in CATALOG {
            for (field, text) in [
                ("tos_note", spec.auth.tos_note),
                ("login_hint", spec.auth.login_hint),
            ] {
                if let Some(t) = text {
                    assert!(
                        t.is_complete(),
                        "`{}`.auth.{field} is missing a locale (en/zh-TW/ja-JP must ALL be set)",
                        spec.id
                    );
                }
            }
        }
    }

    #[test]
    fn locale_text_get_falls_back_to_english() {
        let t = LocaleText { en: "e", zh_tw: "z", ja_jp: "j" };
        assert_eq!(t.get("zh-TW"), "z");
        assert_eq!(t.get("zh_CN"), "z");
        assert_eq!(t.get("ja-JP"), "j");
        assert_eq!(t.get("en-US"), "e");
        assert_eq!(t.get("kl-GL"), "e", "unknown locale falls back to English");
        assert_eq!(t.get(""), "e");
    }

    #[test]
    fn headless_specs_are_coherent() {
        for spec in cli_specs() {
            assert!(
                !spec.headless.args_template.is_empty(),
                "`{}` has a binary but no headless args",
                spec.id
            );
            // A stdin-delivered prompt is legal, but then the template must
            // not contain a stray half-written placeholder.
            for a in spec.headless.args_template {
                assert!(
                    !(a.contains("{prompt") && *a != PROMPT_PLACEHOLDER),
                    "`{}` has a malformed prompt placeholder: {a:?}",
                    spec.id
                );
            }
            if let Some(flag) = spec.headless.model_flag {
                assert!(
                    flag.starts_with('-'),
                    "`{}` model_flag {flag:?} must be a flag",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn model_env_is_only_used_when_there_is_no_flag() {
        // Mistral Vibe has no `--model`; its only channel is the env var.
        let vibe = spec_for("vibe").unwrap();
        assert!(vibe.headless.model_flag.is_none());
        assert_eq!(
            vibe.headless.model_env_pair("devstral"),
            Some(("VIBE_ACTIVE_MODEL", "devstral".to_string()))
        );
        // A runtime WITH a flag must never also set an env var — that would be
        // two sources of truth for the same decision.
        for spec in CATALOG {
            if spec.headless.model_flag.is_some() {
                assert!(
                    spec.headless.model_env_pair("x").is_none(),
                    "`{}` sets a model both by flag and by env",
                    spec.id
                );
            }
        }
        // Kiro has neither — the runtime warns rather than pretending.
        let kiro = spec_for("kiro").unwrap();
        assert!(kiro.headless.model_flag.is_none());
        assert!(kiro.headless.model_env.is_none());
    }

    #[test]
    fn model_args_handles_joined_and_separate_forms() {
        let sep = HeadlessSpec {
            args_template: &["{prompt}"],
            output: OutputFormat::Text,
            model_flag: Some("--model"),
            model_env: None,
            extra_env: &[],
        };
        assert_eq!(sep.model_args("x"), vec!["--model", "x"]);
        assert!(sep.model_args("  ").is_empty(), "blank model ⇒ no flag");

        let joined = HeadlessSpec { model_flag: Some("--model="), ..sep };
        assert_eq!(joined.model_args("x"), vec!["--model=x"]);

        let none = HeadlessSpec { model_flag: None, ..sep };
        assert!(none.model_args("x").is_empty());
    }

    #[test]
    fn prompt_via_stdin_reflects_the_template() {
        let arg = HeadlessSpec {
            args_template: &["-p", "{prompt}"],
            output: OutputFormat::Text,
            model_flag: None,
            model_env: None,
            extra_env: &[],
        };
        assert!(!arg.prompt_via_stdin());
        let stdin = HeadlessSpec { args_template: &["run", "-"], ..arg };
        assert!(stdin.prompt_via_stdin());
    }

    #[test]
    fn credential_paths_are_home_relative() {
        for spec in CATALOG {
            for p in spec.auth.credential_paths {
                assert!(
                    !p.starts_with('/') && !p.starts_with('~'),
                    "`{}` credential path {p:?} must be relative to $HOME",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn api_key_env_names_follow_the_convention() {
        for spec in CATALOG {
            if let Some(name) = spec.auth.api_key_env {
                assert!(
                    name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                        && (name.ends_with("_KEY") || name.ends_with("_TOKEN")),
                    "`{}` api_key_env {name:?} looks wrong",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn runtime_for_model_maps_known_families_and_nothing_else() {
        assert_eq!(runtime_for_model("claude-sonnet-4-6").unwrap().id, "claude");
        assert_eq!(runtime_for_model("gpt-5.4-mini").unwrap().id, "codex");
        assert_eq!(runtime_for_model("o3-mini").unwrap().id, "codex");
        assert_eq!(runtime_for_model("grok-build-0.1").unwrap().id, "grok");
        assert_eq!(
            runtime_for_model("anthropic/claude-sonnet-5").unwrap().id,
            "claude",
            "qualified provider/model form"
        );
        assert!(runtime_for_model("deepseek-v3.2").is_none());
        assert!(runtime_for_model("").is_none());
    }

    #[test]
    fn equal_length_prefix_ties_go_to_the_earlier_catalog_entry() {
        // `gemini` and `antigravity` both declare the `gemini` prefix. Catalog
        // order is preference order, so the Gemini CLI wins — otherwise every
        // `gemini-*` model would auto-align onto `agy`.
        assert_eq!(runtime_for_model("gemini-3-pro-preview").unwrap().id, "gemini");
        assert_eq!(runtime_for_model("gemini-2.5-flash").unwrap().id, "gemini");
    }

    #[test]
    fn multi_vendor_shells_declare_no_family() {
        // Copilot, Cursor and OpenCode all serve OTHER vendors' models, so
        // claiming a family would make them a confident mismatch for their own
        // catalogue and would hijack e.g. `gpt-5` away from codex.
        for id in ["copilot", "cursor", "opencode", "kiro"] {
            assert!(
                spec_for(id).unwrap().model_prefixes.is_empty(),
                "`{id}` must not claim a model family"
            );
        }
        assert_eq!(runtime_for_model("gpt-5").unwrap().id, "codex");
    }

    #[test]
    fn new_runtime_families_resolve() {
        assert_eq!(runtime_for_model("qwen3-coder-plus").unwrap().id, "qwen");
        assert_eq!(runtime_for_model("kimi-for-coding").unwrap().id, "kimi");
        assert_eq!(
            runtime_for_model("kimi-code/kimi-for-coding").unwrap().id,
            "kimi",
            "qualified alias form"
        );
        assert_eq!(runtime_for_model("devstral-small").unwrap().id, "vibe");
    }

    #[test]
    fn openai_compat_claims_no_model_family() {
        let s = spec_for("openai_compat").unwrap();
        assert!(
            s.model_prefixes.is_empty(),
            "openai_compat proxies arbitrary models — claiming a family would \
             make it a confident mismatch for everything else"
        );
    }

    #[test]
    fn id_list_pipe_covers_the_catalog() {
        let list = id_list_pipe();
        for spec in CATALOG {
            assert!(list.contains(spec.id), "`{}` missing from id_list_pipe", spec.id);
        }
    }

    #[test]
    fn install_command_display_never_empty() {
        for spec in CATALOG {
            assert!(!spec.install.command_display().is_empty());
            assert!(!spec.install.kind().is_empty());
        }
    }
}
