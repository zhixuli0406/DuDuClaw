//! Operator Google Chat setup. All persistent writes go through the authenticated
//! gateway Admin RPCs, including MFA and the live account authorization checks.
use std::{ffi::OsString, io::{Read, Write}, path::{Path, PathBuf}, time::Duration};
use clap::{Args, Subcommand};
use duduclaw_core::error::{DuDuClawError as Error, Result};
use duduclaw_gateway::agent_channels::googlechat_config;
use duduclaw_tui::{auth::{AuthClient, LoginMode, disable_core_dumps}, client::{Client, ClientOptions, ConnectionState, RpcReply}, transport::GatewayUrl};
use serde_json::{json, Value};
use zeroize::Zeroizing;

const BOOTSTRAP: &str = include_str!("../../../scripts/googlechat/bootstrap.sh");
const MAX_KEY_BYTES: u64 = 64 * 1024;

pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] = &[
    duduclaw_core::OperatorCommand { name: "duduclaw channels googlechat", path: &["channels", "googlechat"] },
];

#[derive(Subcommand)]
pub enum ChannelsCommands {
    /// 一步建立 Google Chat 應用程式，或匯入已有的服務帳戶金鑰
    Googlechat {
        #[command(subcommand)]
        command: GoogleChatCommands,
    },
}
#[derive(Subcommand)]
pub enum GoogleChatCommands {
    /// 使用自己的 gcloud 登入建立資源，再匯入執行中的 DuDuClaw
    Bootstrap(BootstrapArgs),
    /// 驗證金鑰並經管理員登入匯入；不會將明文金鑰寫入設定
    Import(ImportArgs),
}
#[derive(Args)]
pub struct BootstrapArgs {
    #[arg(long)]
    project_id: Option<String>,
    #[arg(long)]
    endpoint_url: Option<String>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    app_name: Option<String>,
    #[arg(long)]
    out: Option<PathBuf>,
    /// 只列出指令；不呼叫 Google、不建立金鑰、不匯入
    #[arg(long)]
    dry_run: bool,
}
#[derive(Args)]
pub struct ImportArgs {
    #[arg(long)]
    project_number: String,
    /// 金鑰須為一般檔案；Windows 必須存放在自己的使用者設定檔目錄並限制存取權限
    #[arg(long)]
    key_file: PathBuf,
    #[arg(long)]
    agent: Option<String>,
}

fn operator_only() -> Result<()> {
    if !crate::ai_session_guard::command_markers().is_empty() {
        return Err(Error::Security("Google Chat 設定僅限操作者在自己的終端機執行，AI 工作階段不可執行。".into()));
    }
    Ok(())
}
fn validate_agent(agent: Option<&str>) -> Result<()> {
    if agent.is_some_and(|id| !duduclaw_core::is_valid_agent_id(id)
        || duduclaw_core::is_system_sender(id) || duduclaw_core::is_reserved_queue_sender(id)) {
        return Err(Error::Config("員工 ID 格式不正確。".into()));
    }
    Ok(())
}
pub async fn run(home: &Path, command: ChannelsCommands) -> Result<()> {
    // Refuse before inspecting files, starting gcloud or acquiring credentials.
    operator_only()?;
    disable_core_dumps().map_err(client_error)?;
    let ChannelsCommands::Googlechat { command } = command;
    match command {
        GoogleChatCommands::Import(args) => import(home, args).await,
        GoogleChatCommands::Bootstrap(args) => bootstrap(home, args).await,
    }
}
fn script_args(args: &BootstrapArgs) -> Result<Vec<OsString>> {
    validate_agent(args.agent.as_deref())?;
    let has_controls = |value: &str| value.chars().any(|c| c.is_control());
    if args.app_name.as_deref().is_some_and(|name| has_controls(name) || name.chars().count() > 25)
        || args.out.as_ref().is_some_and(|out| out.to_str().is_none_or(has_controls)) {
        return Err(Error::Config("應用程式名稱最多 25 字；名稱與輸出路徑不可包含控制字元。".into()));
    }
    let mut values = Vec::new();
    if let Some(out) = &args.out { values.extend([OsString::from("--out"), out.as_os_str().to_owned()]); }
    for (flag, value) in [("--project-id", &args.project_id), ("--endpoint-url", &args.endpoint_url), ("--agent", &args.agent), ("--app-name", &args.app_name)] {
        if let Some(value) = value { values.extend([flag.into(), value.into()]); }
    }
    if args.dry_run { values.push("--dry-run".into()); }
    Ok(values)
}
fn resolve_bootstrap_defaults(args: &mut BootstrapArgs) -> PathBuf {
    let project = args.project_id.get_or_insert_with(|| {
        let suffix: String = uuid::Uuid::new_v4().simple().to_string().chars().take(6).collect();
        format!("duduclaw-chat-{suffix}")
    });
    args.out.get_or_insert_with(|| PathBuf::from(format!("googlechat-setup-{project}"))).clone()
}
async fn bootstrap(home: &Path, mut args: BootstrapArgs) -> Result<()> {
    let out = resolve_bootstrap_defaults(&mut args);
    let argv = script_args(&args)?;
    let mut script = tempfile::Builder::new().prefix("duduclaw-googlechat-").suffix(".sh").tempfile()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        script.as_file().set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    script.write_all(BOOTSTRAP.as_bytes())?;
    script.flush()?;
    run_bootstrap_script(script.path(), &argv).await?;
    if args.dry_run { return Ok(()); }
    let result = serde_json::from_str(&read_bounded(&out.join("bootstrap-result.json"))?)
        .map_err(|_| bootstrap_review_error(args.project_id.as_deref()))?;
    let validated = validate_bootstrap_result(result, &out, args.project_id.as_deref())?;
    let result = &validated.0;
    finish_bootstrap_import(&validated, args.project_id.as_deref().unwrap(), args.agent.as_deref(), |raw| import_bytes(home, ImportArgs {
        project_number: result.project_number.clone(), key_file: result.key_file.clone(),
        agent: args.agent.clone(),
    }, raw)).await
}

async fn run_bootstrap_script(script: &Path, argv: &[OsString]) -> Result<()> {
    let status = tokio::process::Command::new("bash")
        .arg(script).args(argv)
        .stdin(std::process::Stdio::inherit()).stdout(std::process::Stdio::inherit())
        .status().await.map_err(|_| Error::Config("找不到 Bash；請安裝 Bash 或使用 Cloud Shell 執行設定腳本。".into()))?;
    if !status.success() {
        return Err(Error::Config("Google Chat 資源設定失敗，尚未匯入。請依上方提示修正後重試。".into()));
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapResult {
    project_number: String,
    key_file: PathBuf,
    project_id: String,
    service_account_email: String,
    key_id: String,
    created_this_run: bool,
    #[serde(default, rename = "agent")]
    _agent: String,
}

struct ValidatedBootstrapResult(BootstrapResult, ValidatedKey);

// Import the validated snapshot without reopening the path.
struct ValidatedKey {
    raw: Zeroizing<String>,
    _file: std::fs::File,
}

fn valid_project_id(project: &str) -> bool {
    let bytes = project.as_bytes();
    (6..=30).contains(&bytes.len()) && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}
fn has_terminal_controls(value: &str) -> bool {
    value.chars().any(|c| {
        let n = c as u32;
        c.is_control() || matches!(n, 0x00ad | 0x0600..=0x0605 | 0x061c | 0x06dd | 0x070f
            | 0x0890..=0x0891 | 0x08e2 | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e
            | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff | 0xfff9..=0xfffb
            | 0x110bd | 0x110cd | 0x13430..=0x1343f | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f)
    })
}
fn bootstrap_review_error(expected_project: Option<&str>) -> Error {
    // Only trusted invocation data may appear in manual recovery instructions.
    let command = match expected_project.filter(|project| valid_project_id(project)) {
        Some(project) => format!("gcloud iam service-accounts keys list --iam-account=duduclaw-chat@{project}.iam.gserviceaccount.com --project={project}"),
        None => "gcloud projects list".into(),
    };
    Error::Config(format!("Google Chat 設定結果或金鑰不符，尚未接管或匯入；不會撤銷金鑰或刪除檔案。請先人工檢查：{command}"))
}
fn validate_bootstrap_result(result: BootstrapResult, out: &Path, expected_project: Option<&str>) -> Result<ValidatedBootstrapResult> {
    let check = || -> Result<ValidatedBootstrapResult> {
        let path_text = result.key_file.to_string_lossy();
        if !(6..=20).contains(&result.project_number.len()) || !result.project_number.bytes().all(|b| b.is_ascii_digit())
            || result.project_id.len() > 128 || result.service_account_email.len() > 254
            || result.key_id.len() > 128 || path_text.len() > 4096
            || [result.project_number.as_str(), result.project_id.as_str(), result.service_account_email.as_str(), result.key_id.as_str(), result._agent.as_str()]
                .iter().any(|value| has_terminal_controls(value))
            || result.key_id.len() != 40 || !result.key_id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_project_id(&result.project_id)
            || expected_project.is_some_and(|project| project != result.project_id)
            || result.service_account_email != format!("duduclaw-chat@{}.iam.gserviceaccount.com", result.project_id) {
            return Err(bootstrap_review_error(expected_project));
        }
        let root = std::fs::canonicalize(out)?;
        let parent = std::fs::canonicalize(result.key_file.parent().unwrap_or(Path::new(".")))?;
        if !parent.starts_with(&root) {
            return Err(bootstrap_review_error(expected_project));
        }
        let name = result.key_file.file_name().ok_or_else(|| bootstrap_review_error(expected_project))?.to_owned();
        let mut directory_options = std::fs::OpenOptions::new();
        directory_options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            directory_options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Windows requires BACKUP_SEMANTICS to open a directory handle.
            directory_options.custom_flags(0x0200_0000 | 0x0020_0000);
        }
        let directory = directory_options.open(&parent)?;
        #[cfg(unix)]
        let file = {
            use std::os::{fd::{AsRawFd, FromRawFd}, unix::ffi::OsStrExt};
            let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| bootstrap_review_error(expected_project))?;
            let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
            if fd < 0 { return Err(std::io::Error::last_os_error().into()); }
            unsafe { std::fs::File::from_raw_fd(fd) }
        };
        #[cfg(not(unix))]
        let file = open_key(&result.key_file)?;
        let key_snapshot = read_key_file(file)?;
        let raw = &key_snapshot.raw;
        let key: Value = serde_json::from_str(&raw).map_err(|_| bootstrap_review_error(expected_project))?;
        if key.get("project_id").and_then(Value::as_str) != Some(result.project_id.as_str())
            || key.get("client_email").and_then(Value::as_str) != Some(result.service_account_email.as_str())
            || key.get("private_key_id").and_then(Value::as_str) != Some(result.key_id.as_str()) {
            return Err(bootstrap_review_error(expected_project));
        }
        Ok(ValidatedBootstrapResult(result, key_snapshot))
    };
    check().map_err(|_| bootstrap_review_error(expected_project))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

async fn finish_bootstrap_import<'a, F: std::future::Future<Output = Result<()>>>(
    validated: &'a ValidatedBootstrapResult,
    project: &str,
    agent: Option<&str>,
    import: impl FnOnce(&'a str) -> F,
) -> Result<()> {
    let result = &validated.0;
    import(&validated.1.raw).await.map_err(|error| {
        // Recovery instructions use the invocation's project/SA and the key ID
        // from the already validated in-memory key, never script revoke targets.
        let key: Value = serde_json::from_str(&validated.1.raw).expect("validated key JSON");
        let key_id = key["private_key_id"].as_str().expect("validated key ID");
        let mut retry = format!("duduclaw channels googlechat import --project-number {} --key-file {:?}",
            shell_quote(&result.project_number), result.key_file);
        if let Some(agent) = agent { retry.push_str(&format!(" --agent {}", shell_quote(agent))); }
        let key_status = if result.created_this_run { "本次已建立新的服務帳戶金鑰，仍然有效。" }
            else { "本次沿用的服務帳戶金鑰仍然有效。" };
        Error::Config(format!("{error}\n金鑰檔：{:?}\n{key_status} CLI 未撤銷金鑰，也未刪除檔案。\n重試匯入：\n{retry}\n或手動撤銷：\ngcloud iam service-accounts keys delete {key_id} --iam-account duduclaw-chat@{project}.iam.gserviceaccount.com --project {project}", result.key_file))
    })
}

fn read_bounded(path: &Path) -> Result<Zeroizing<String>> {
    Ok(read_key_file(open_key(path)?)?.raw)
}
fn open_key(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // NONBLOCK also prevents a hostile FIFO from hanging before fstat.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself so the handle metadata cannot hide a link.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    Ok(options.open(path)?)
}
fn read_key_file(file: std::fs::File) -> Result<ValidatedKey> {
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o7777 != 0o600 || metadata.nlink() != 1 {
            return Err(Error::Config("金鑰檔必須由目前使用者擁有、只有一個連結，且權限為 0600；請執行 chmod 600 後再試。".into()));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_file() {
            return Err(Error::Config("金鑰檔必須是一般檔案，不可為重新解析點；請存放在自己的使用者設定檔目錄並限制存取權限。".into()));
        }
    }
    if !metadata.is_file() || metadata.len() > MAX_KEY_BYTES {
        return Err(Error::Config("金鑰檔必須是小於 64 KiB 的一般 JSON 檔案。".into()));
    }
    let mut contents = Zeroizing::new(String::new());
    (&file).take(MAX_KEY_BYTES + 1).read_to_string(&mut contents)?;
    if contents.len() as u64 > MAX_KEY_BYTES { return Err(Error::Config("金鑰檔超過 64 KiB。".into())); }
    Ok(ValidatedKey { raw: contents, _file: file })
}
fn import_request(args: &ImportArgs, raw: &str) -> Result<(&'static str, Value)> {
    validate_agent(args.agent.as_deref())?;
    let project = googlechat_config::validate_project_number(&args.project_number).map_err(Error::Config)?;
    googlechat_config::validate_service_account_json(raw).map_err(Error::Config)?;
    Ok(match &args.agent {
        Some(agent) => ("agents.channels.googlechat.set", json!({"agent_id": agent, "project_number": project, "service_account_json": raw})),
        None => ("channels.add", json!({"type": "googlechat", "config": {"token": raw, "secret": project}})),
    })
}
async fn import(home: &Path, args: ImportArgs) -> Result<()> {
    let raw = read_bounded(&args.key_file)?;
    import_bytes(home, args, &raw).await
}
async fn import_bytes(home: &Path, args: ImportArgs, raw: &str) -> Result<()> {
    let (method, params) = import_request(&args, raw)?;
    let auth = AuthClient::new(GatewayUrl::for_home(home).map_err(client_error)?).map_err(client_error)?;
    let session = auth.login(LoginMode::Personal, &mut crate::tui_cmd::TerminalPrompt).await.map_err(client_error)?;
    let client = Client::start(auth, session, ClientOptions::default()).map_err(client_error)?;
    let mut status = client.connection();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match status.borrow().clone() {
                ConnectionState::Connected(_) => return Ok(()),
                ConnectionState::LoginRequired(_) => return Err(Error::Config("請以管理員帳號重新登入後匯入。".into())),
                _ => {}
            }
            status.changed().await.map_err(|_| Error::Config("閘道連線中斷，尚未匯入。".into()))?;
        }
    }).await.map_err(|_| Error::Config("無法連上 DuDuClaw；請先啟動閘道，再執行 import。".into()))??;
    // The RPC validates and exchanges the candidate key BEFORE committing it.
    match client.call_detailed(method, params, Duration::from_secs(90)).await.map_err(client_error)? {
        RpcReply::Accepted(_) => {
            println!("Google Chat 設定已匯入；請完成 Google Cloud 的 Chat 應用程式設定。\nhttps://console.cloud.google.com/apis/api/chat.googleapis.com/hangouts-chat?project={}", args.project_number.trim());
            Ok(())
        }
        RpcReply::Rejected { message, .. } => Err(Error::Config(message.plain())),
    }
}
fn client_error(error: duduclaw_tui::Error) -> Error {
    Error::Config(format!("Google Chat 匯入未完成（需要執行中的閘道與管理員登入）：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[derive(Parser)]
    struct TestCli { #[command(subcommand)] command: ChannelsCommands }
    #[cfg(unix)]
    fn bootstrap_fixture(dir: &Path) -> BootstrapResult {
        use std::os::unix::fs::PermissionsExt;
        let key_file = dir.join("key.json");
        std::fs::write(&key_file, json!({"project_id": "demo-chat-123",
            "client_email": "duduclaw-chat@demo-chat-123.iam.gserviceaccount.com",
            "private_key_id": "a".repeat(40)}).to_string()).unwrap();
        std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        BootstrapResult { project_number: "123456789".into(), key_file,
            project_id: "demo-chat-123".into(), service_account_email: "duduclaw-chat@demo-chat-123.iam.gserviceaccount.com".into(),
            key_id: "a".repeat(40), created_this_run: true, _agent: String::new() }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn googlechat_bootstrap_untrusted_results_preserve_files() {
        use std::{cell::Cell, os::unix::fs::symlink};
        for tamper in ["key_id", "uppercase_id", "short_id", "project", "sa", "outside", "symlink", "private_key_id", "key_project", "key_email"] {
            let dir = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let mut result = bootstrap_fixture(dir.path());
            let original = result.key_file.clone();
            match tamper {
                "key_id" => result.key_id = "--all".into(),
                "uppercase_id" => result.key_id = "A".repeat(40),
                "short_id" => result.key_id = "a".repeat(39),
                "project" => result.project_id = "other-project".into(),
                "sa" => result.service_account_email = "other@demo-chat-123.iam.gserviceaccount.com".into(),
                "outside" => result.key_file = bootstrap_fixture(outside.path()).key_file,
                "symlink" => {
                    result.key_file = dir.path().join("link.json");
                    symlink(&original, &result.key_file).unwrap();
                }
                field => {
                    let mut key: Value = serde_json::from_str(&std::fs::read_to_string(&original).unwrap()).unwrap();
                    key[match field { "key_project" => "project_id", "key_email" => "client_email", _ => "private_key_id" }] = json!("mismatch");
                    std::fs::write(&original, key.to_string()).unwrap();
                }
            }
            let before = std::fs::read(&result.key_file).unwrap();
            let imported = Cell::new(false);
            let key_file = result.key_file.clone();
            let outcome = match validate_bootstrap_result(result, dir.path(), Some("demo-chat-123")) {
                Ok(validated) => finish_bootstrap_import(&validated, "demo-chat-123", None, |_| async {
                    imported.set(true);
                    Err(Error::Config("RPC refused".into()))
                }).await,
                Err(error) => Err(error),
            };
            let error = outcome.unwrap_err().to_string();
            assert!(error.contains("人工檢查"), "{tamper}: {error}");
            assert!(error.contains("gcloud iam service-accounts keys list"));
            assert!(!imported.get(), "{tamper}");
            assert!(original.exists(), "{tamper}");
            assert_eq!(std::fs::read(&key_file).unwrap(), before, "{tamper}");
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn googlechat_bootstrap_import_failure_preserves_key_and_prints_manual_recovery() {
        // Isolate PATH in a child test process so other parallel tests are unaffected.
        if std::env::var_os("GOOGLECHAT_TEST_GCLOUD_LOG").is_none() {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("gcloud.log");
            let gcloud = dir.path().join("gcloud");
            std::fs::write(&gcloud, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$GOOGLECHAT_TEST_GCLOUD_LOG\"\nexit 0\n").unwrap();
            std::fs::set_permissions(&gcloud, std::fs::Permissions::from_mode(0o700)).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "googlechat_cmd::tests::googlechat_bootstrap_import_failure_preserves_key_and_prints_manual_recovery", "--nocapture"])
                .env("PATH", dir.path()).env("GOOGLECHAT_TEST_GCLOUD_LOG", &log)
                .status().unwrap();
            assert!(status.success());
            assert!(!log.exists(), "CLI import failure must not invoke gcloud");
            return;
        }
        for created in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let mut result = bootstrap_fixture(dir.path());
            result.created_this_run = created;
            let key_file = result.key_file.clone();
            let before = std::fs::read(&key_file).unwrap();
            let validated = validate_bootstrap_result(result, dir.path(), Some("demo-chat-123")).unwrap();
            let error = finish_bootstrap_import(&validated, "demo-chat-123", Some("alice"), |_| async {
                Err(Error::Config("RPC refused: Admin required".into()))
            }).await.unwrap_err().to_string();
            assert_eq!(std::fs::read(&key_file).unwrap(), before);
            assert!(error.contains(&key_file.display().to_string()));
            assert!(error.contains("仍然有效"));
            assert!(error.contains("duduclaw channels googlechat import --project-number '123456789' --key-file"));
            assert!(error.contains("--agent 'alice'"));
            assert!(error.contains(&format!("gcloud iam service-accounts keys delete {} --iam-account duduclaw-chat@demo-chat-123.iam.gserviceaccount.com --project demo-chat-123", "a".repeat(40))));
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn googlechat_bootstrap_rejects_terminal_injection_and_escapes_paths() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut injected = bootstrap_fixture(dir.path());
        injected.project_number = "\u{001b}]52;clipboard\u{0007}".into();
        assert!(validate_bootstrap_result(injected, dir.path(), Some("demo-chat-123")).is_err());

        let dir = tempfile::tempdir().unwrap();
        let mut result = bootstrap_fixture(dir.path());
        let escaped_name = "key\n\u{001b}[31m.json";
        result.key_file = dir.path().join(escaped_name);
        std::fs::write(&result.key_file, json!({"project_id": "demo-chat-123",
            "client_email": "duduclaw-chat@demo-chat-123.iam.gserviceaccount.com",
            "private_key_id": "a".repeat(40)}).to_string()).unwrap();
        std::fs::set_permissions(&result.key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let validated = validate_bootstrap_result(result, dir.path(), Some("demo-chat-123")).unwrap();
        let error = finish_bootstrap_import(&validated, "demo-chat-123", None, |_| async {
            Err(Error::Config("RPC refused".into()))
        }).await.unwrap_err().to_string();
        assert!(error.contains("\\n\\u{1b}[31m"), "{error}");
        assert!(!error.contains(escaped_name), "{error}");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn googlechat_replaced_key_uses_validated_bytes_and_preserves_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let result = bootstrap_fixture(dir.path());
        let path = result.key_file.clone();
        let original = std::fs::read_to_string(&path).unwrap();
        let validated = validate_bootstrap_result(result, dir.path(), Some("demo-chat-123")).unwrap();
        std::fs::rename(&path, dir.path().join("original.json")).unwrap();
        std::fs::write(&path, "replacement must survive").unwrap();
        let outcome = finish_bootstrap_import(&validated, "demo-chat-123", None, |raw| async move {
            assert_eq!(raw, original);
            Err(Error::Config("RPC refused".into()))
        }).await;
        assert!(outcome.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement must survive");
    }
    #[test]
    fn googlechat_result_rejects_unknown_fields() {
        let value = json!({"project_number": "123456789", "key_file": "key.json",
            "project_id": "demo-chat-123", "service_account_email": "sa", "key_id": "a".repeat(40),
            "created_this_run": true, "agent": "", "unexpected": true});
        assert!(serde_json::from_value::<BootstrapResult>(value).is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn googlechat_result_requires_successful_script_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("failed.sh");
        std::fs::write(&script, "exit 1").unwrap();
        assert!(run_bootstrap_script(&script, &[]).await.is_err());
    }
    #[test]
    fn googlechat_cli_parses_http_flows_and_refuses_unknown_connection() {
        assert!(TestCli::try_parse_from(["channels", "googlechat", "import", "--project-number", "123456789", "--key-file", "key.json", "--agent", "alice"]).is_ok());
        assert!(TestCli::try_parse_from(["channels", "googlechat", "bootstrap", "--dry-run", "--endpoint-url", "https://chat.example.com/webhook/googlechat"]).is_ok());
        assert!(TestCli::try_parse_from(["channels", "googlechat", "bootstrap", "--connection", "typo"]).is_err());
    }
    #[test]
    fn googlechat_operator_refuses_even_empty_identity() {
        for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            let _guard = crate::ai_session_guard::test_session(&[(*marker, "")]);
            assert!(operator_only().is_err());
        }
        assert!(operator_only().is_ok());
    }
    #[test]
    fn googlechat_bash_lane_refuses_employee_and_untrusted_setup() {
        for command in ["duduclaw channels googlechat bootstrap --endpoint-url https://chat.example.com/webhook/googlechat", "duduclaw-pro channels googlechat import --project-number 123456789 --key-file key.json", "bash -c 'duduclaw channels googlechat bootstrap'"] {
            for caller in [duduclaw_core::HookCaller::Agent("alice".into()), duduclaw_core::HookCaller::Untrusted("unknown".into())] {
                assert!(duduclaw_core::bash_operator_command_decision(command, &caller, OPERATOR_COMMANDS).is_some());
            }
        }
    }
    #[test]
    fn googlechat_script_arguments_are_not_shell_interpolated() {
        let args = BootstrapArgs { project_id: None, endpoint_url: None, agent: Some("alice".into()), app_name: Some("$(touch /tmp/never) ' app".into()), out: Some("dir with spaces".into()), dry_run: true };
        let argv = script_args(&args).unwrap();
        assert!(argv.contains(&OsString::from("$(touch /tmp/never) ' app")));
        assert!(argv.contains(&OsString::from("dir with spaces")));
        assert_eq!(argv.last().unwrap(), "--dry-run");
    }
    #[test]
    fn googlechat_bootstrap_defaults_follow_each_project_and_preserve_explicit_output() {
        let new_args = || BootstrapArgs { project_id: None, endpoint_url: None, agent: None, app_name: None, out: None, dry_run: true };
        let mut first = new_args();
        let first_out = resolve_bootstrap_defaults(&mut first);
        let project = first.project_id.as_ref().unwrap();
        assert_eq!(project.len(), "duduclaw-chat-".len() + 6);
        assert_eq!(first_out, PathBuf::from(format!("googlechat-setup-{project}")));
        assert_eq!(resolve_bootstrap_defaults(&mut first), first_out);
        let mut second = new_args();
        second.project_id = Some("another-chat-project".into());
        assert_eq!(resolve_bootstrap_defaults(&mut second), PathBuf::from("googlechat-setup-another-chat-project"));
        assert_ne!(second.out, first.out);
        second.out = Some("explicit-output".into());
        assert_eq!(resolve_bootstrap_defaults(&mut second), PathBuf::from("explicit-output"));
    }
    #[test]
    fn googlechat_import_rejects_invalid_project_key_and_agent_before_network() {
        let mut args = ImportArgs { project_number: "project-id".into(), key_file: "unused".into(), agent: None };
        assert!(import_request(&args, "{}").is_err());
        args.project_number = "123456789".into();
        assert!(import_request(&args, "{}").is_err());
        args.agent = Some("../alice".into());
        assert!(import_request(&args, "{}").is_err());
    }
    #[test]
    fn googlechat_key_read_is_bounded() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![b'a'; MAX_KEY_BYTES as usize + 1]).unwrap();
        assert!(read_bounded(file.path()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn googlechat_key_read_refuses_loose_permissions_links_and_nonregular_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key.json");
        std::fs::write(&key, "{}").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_bounded(&key).unwrap().as_str(), "{}");
        for mode in [0o644, 0o640, 0o660, 0o400, 0o4600] {
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(mode)).unwrap();
            // Some filesystems strip setuid: validate the mode that actually landed.
            let actual = std::fs::metadata(&key).unwrap().permissions().mode() & 0o7777;
            if mode == 0o4600 && actual == 0o600 {
                assert_eq!(read_bounded(&key).unwrap().as_str(), "{}");
            } else {
                assert!(read_bounded(&key).unwrap_err().to_string().contains("chmod 600"));
            }
        }
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("symlink.json");
        symlink(&key, &link).unwrap();
        assert!(read_bounded(&link).is_err());
        let hardlink = dir.path().join("hardlink.json");
        std::fs::hard_link(&key, &hardlink).unwrap();
        assert!(read_bounded(&key).is_err());
        assert!(read_bounded(&hardlink).is_err());
        assert!(read_bounded(dir.path()).is_err());
    }
    #[test]
    fn googlechat_bootstrap_rejects_control_characters_before_spawn() {
        let mut args = BootstrapArgs { project_id: None, endpoint_url: None, agent: None, app_name: None, out: None, dry_run: true };
        for value in ["bad\0name", "bad\nname", "bad\u{85}name", "abcdefghijklmnopqrstuvwxyz"] {
            args.app_name = Some(value.into());
            assert!(script_args(&args).is_err());
        }
        args.app_name = None;
        for value in ["bad\0path", "bad\npath", "bad\u{85}path"] {
            args.out = Some(value.into());
            assert!(script_args(&args).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn googlechat_embedded_dry_run_needs_no_credentials_or_files() {
        let result = std::process::Command::new("bash").arg("-c").arg(BOOTSTRAP).arg("bootstrap").args(["--dry-run", "--project-id", "test-chat-project", "--endpoint-url", "https://chat.example.com/webhook/googlechat"]).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(output.contains("HTTP endpoint URL:"));
        assert!(output.contains("test-chat-project"));
    }
}
