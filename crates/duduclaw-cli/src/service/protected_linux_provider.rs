//! `duduclaw service provider set --protected-linux` (R8r-2): host root writes
//! (or, with `--clear`, removes) an isolated employee's model connection
//! document. spawnd stores it in the employee's own home
//! (`/home/employee/.duduclaw-api/providers.json`, readable by that
//! employee's runtime); this command only reaches spawnd's allocation socket
//! as uid 0 / gid 0 with a schema-5 `provision_providers`.
//!
//! The document comes from standard input ONLY (never argv, the environment
//! or a path), at most 65,536 bytes, in a zeroizing buffer, validated with the
//! employee runtime's own rules (`duduclaw_api_worker::Credential::validate`)
//! and never printed, logged or echoed: every message is a fixed line.
// Off Linux the command is a stub; its pure parts stay compiled and tested.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use super::{operator_gate, refused, Result};
use std::io::Read;
use zeroize::{Zeroize, Zeroizing};

/// Largest document read from standard input (bytes); the root side's limit.
pub(crate) const MAX_DOCUMENT: usize = 65_536;

#[derive(clap::Subcommand)]
pub(crate) enum ProviderCommand {
    /// Write a protected employee's model connection document, read from
    /// standard input only (or remove it with --clear)
    #[command(group(clap::ArgGroup::new("employee_target").required(true).multiple(false).args(["employee_id", "employee"])))]
    Set {
        #[arg(long, required = true)]
        protected_linux: bool,
        /// The employee id (32 lowercase hex digits)
        #[arg(long)]
        employee_id: Option<String>,
        /// The employee label recorded when it was allocated
        #[arg(long)]
        employee: Option<String>,
        /// Remove the document instead of writing one (standard input is not read)
        #[arg(long)]
        clear: bool,
    },
}

const TTY_HINT: &str = "請由標準輸入提供模型連線資料，例如：sudo duduclaw service provider set --protected-linux --employee <員工名稱> < providers.json；\
格式：{\"providers\":[{\"name\":\"<名稱>\",\"api_key\":\"<金鑰>\",\"base_url\":\"https://<主機>/v1\"}]}";
const TOO_LARGE: &str = "模型連線資料超過 65,536 位元組上限，未送出。";
const EMPTY: &str = "標準輸入沒有內容，未送出。";
const READ_FAILED: &str = "無法讀取標準輸入，未送出。";
const INVALID: &str = "模型連線資料格式不正確（需要 1 至 32 個名稱不重複的提供者；端點必須是 https，且不含帳號密碼、查詢字串或片段；字串中不可使用 JSON 跳脫字元 \\），未送出。";
const BAD_ID: &str = "--employee-id 必須是 32 個小寫十六進位字元。";
const BAD_LABEL: &str = "--employee 必須是 1 至 64 個英文字母、數字、- 或 _。";
const ONE_TARGET: &str = "請指定 --employee-id 或 --employee 其中之一。";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const ROOT: &str = "需要 root 權限（uid 0、gid 0），請使用 sudo / Root privileges are required; use sudo.";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const UNREACHABLE: &str = "無法連線到隔離服務（spawnd 未執行或通道無法使用），未變更。";
/// F3: the request was sent but no confirmed answer came back (no reply, a
/// timeout, a garbled reply, or root's own `isolation_unavailable`, which its
/// 4 s bound also answers): the write may or may not have happened.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const UNCONFIRMED: &str = "結果未確認：請求已送出，但沒有收到隔離服務的確認，檔案可能已變更也可能沒有；再次執行同一指令是安全的（每次執行都會重新寫入或移除該檔案）。";
/// F3: the fixed line of a transport failure, by whether the request had
/// been fully sent.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn transport_line(sent: bool) -> &'static str { if sent { UNCONFIRMED } else { UNREACHABLE } }

/// The employee as the operator named it (validated; shown back as given).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target { Id(String), Label(String) }
impl Target {
    fn shown(&self) -> &str { match self { Self::Id(id) => id, Self::Label(label) => label } }
}
fn target(employee_id: Option<&str>, employee: Option<&str>) -> Result<Target> {
    match (employee_id, employee) {
        (Some(id), None) => {
            if id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) && id.bytes().any(|b| b != b'0') {
                Ok(Target::Id(id.into()))
            } else { Err(refused(BAD_ID)) }
        },
        (None, Some(label)) => {
            if !label.is_empty() && label.len() <= 64 && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                Ok(Target::Label(label.into()))
            } else { Err(refused(BAD_LABEL)) }
        },
        _ => Err(refused(ONE_TARGET)),
    }
}

/// Standard input only: a terminal is refused with the fixed shape hint;
/// more than [`MAX_DOCUMENT`] bytes or nothing at all is refused. The buffer
/// is allocated once (no reallocation copies) and zeroized when dropped.
pub(crate) fn read_document(input: &mut dyn Read, is_terminal: bool) -> Result<Zeroizing<Vec<u8>>> {
    if is_terminal { return Err(refused(TTY_HINT)); }
    let mut buffer = Zeroizing::new(vec![0u8; MAX_DOCUMENT + 1]);
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {},
            Err(_) => return Err(refused(READ_FAILED)),
        }
    }
    if filled > MAX_DOCUMENT { return Err(refused(TOO_LARGE)); }
    if filled == 0 { return Err(refused(EMPTY)); }
    buffer.truncate(filled);
    Ok(buffer)
}

/// What a success line may show: each provider's name and its endpoint's
/// `scheme://host[:port]` (never the key, path or query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary { pub providers: Vec<(String, String)> }

#[cfg(test)]
thread_local! {
    /// Test observation: how many [`Secret`]s were dropped wiped (empty after zeroize).
    pub(crate) static WIPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// F2: one string field of the document (`name`, `api_key`, `base_url`),
/// owned only inside a zeroizing buffer and wiped when dropped — on EVERY
/// path: success, a refusal, a parse error after earlier providers were
/// already built (serde drops the partially built `Vec<Provider>`, which
/// drops each element), and an unwind.
///
/// Only a string BORROWED from the input (no JSON escape in it) is accepted:
/// it is copied once, at its exact length, straight into the zeroizing
/// buffer. A string with an escape sequence is unescaped by serde_json into
/// its own scratch buffer, which is never zeroized (freed with the
/// deserializer) — the same residual the server documents for its frames.
/// As on the server, such a string is refused (one fixed line, never
/// echoed): an accepted document never leaves that copy, and only a refused
/// one can (the residual). This refuses MORE than the employee runtime (its
/// parser accepts escapes): every document this command sends is one the
/// runtime accepts, never the other way round.
pub(crate) struct Secret(Zeroizing<String>);
impl Secret {
    fn as_str(&self) -> &str { self.0.as_str() }
}
impl Drop for Secret {
    fn drop(&mut self) {
        // `Zeroizing` would wipe it on its own drop as well; doing it here
        // keeps the wipe observable in tests.
        self.0.zeroize();
        #[cfg(test)]
        WIPED.with(|wiped| wiped.set(wiped.get() + usize::from(self.0.is_empty())));
    }
}
impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Secret;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("a string without escape sequences") }
            fn visit_borrowed_str<E: serde::de::Error>(self, text: &'de str) -> std::result::Result<Secret, E> {
                Ok(Secret(Zeroizing::new(text.to_owned())))
            }
            /// Unescaped into the parser's scratch buffer: refused, never quoted.
            fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Secret, E> { Err(E::custom("escaped string")) }
        }
        deserializer.deserialize_str(Visitor)
    }
}
/// The runtime's document shape (`duduclaw_api_worker::Credentials` /
/// `Credential`: the same fields, unknown fields refused, `protocol`
/// optional), with every string a [`Secret`].
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Document { providers: Vec<Provider> }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Provider { name: Secret, api_key: Secret, base_url: Secret,
    #[serde(default)] protocol: duduclaw_api_worker::vendor::Protocol }
/// The runtime's `Credential`, built only to call its own `validate`; its
/// strings are wiped when this guard drops (every path, unwind included).
struct Checked(duduclaw_api_worker::Credential);
impl Checked {
    fn new(provider: &Provider) -> Self {
        Self(duduclaw_api_worker::Credential { name: provider.name.as_str().to_owned(), api_key: provider.api_key.as_str().to_owned(),
            base_url: provider.base_url.as_str().to_owned(), protocol: provider.protocol })
    }
}
impl Drop for Checked {
    fn drop(&mut self) {
        self.0.api_key.zeroize();
        self.0.base_url.zeroize();
        self.0.name.zeroize();
    }
}
/// The employee runtime's rules for the document (`Credentials::select`):
/// 1..=32 providers, each passing `Credential::validate`, names unique; and
/// no JSON escape in any string ([`Secret`]). Every parsed string is wiped on
/// every path; a failure is one fixed line that contains nothing of the input.
/// Residuals: the URL parser's own serialisation of `base_url` (here and in
/// the runtime's `validate`; an endpoint, refused if it carries a user or a
/// password) and the printed names/origins are not wiped.
pub(crate) fn validate(document: &[u8]) -> Result<Summary> {
    let parsed: Document = serde_json::from_slice(document).map_err(|_| refused(INVALID))?;
    summarize(&parsed)
}
fn summarize(parsed: &Document) -> Result<Summary> {
    if parsed.providers.is_empty() || parsed.providers.len() > 32 { return Err(refused(INVALID)); }
    let mut seen = std::collections::BTreeSet::new();
    let mut providers = Vec::with_capacity(parsed.providers.len());
    for credential in &parsed.providers {
        let checked = Checked::new(credential);
        let valid = checked.0.validate().is_ok();
        drop(checked);
        if !valid || !seen.insert(credential.name.as_str()) { return Err(refused(INVALID)); }
        let url = url::Url::parse(credential.base_url.as_str()).map_err(|_| refused(INVALID))?;
        let host = url.host_str().ok_or_else(|| refused(INVALID))?;
        let origin = match url.port() {
            Some(port) => format!("{}://{host}:{port}", url.scheme()),
            None => format!("{}://{host}", url.scheme()),
        };
        providers.push((credential.name.as_str().to_owned(), origin));
    }
    Ok(Summary { providers })
}

/// The one success line: the employee as named, the number of providers,
/// each provider's name and origin.
pub(crate) fn success_line(target: &Target, summary: Option<&Summary>) -> String {
    match summary {
        Some(summary) => {
            let list = summary.providers.iter().map(|(name, origin)| format!("{name}（{origin}）")).collect::<Vec<_>>().join("、");
            format!("已寫入員工 {} 的模型連線資料：{} 個提供者：{list}。", target.shown(), summary.providers.len())
        },
        None => format!("已移除員工 {} 的模型連線資料。", target.shown()),
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn authorize(uid: u32, gid: u32) -> Result<()> {
    operator_gate()?;
    if uid != 0 || gid != 0 { return Err(refused(ROOT)); }
    Ok(())
}

/// One fixed line per root refusal.
#[cfg(target_os = "linux")]
fn refusal_line(code: duduclaw_migration::allocation::providers::ProviderErrorCode) -> &'static str {
    use duduclaw_migration::allocation::providers::ProviderErrorCode as Code;
    match code {
        Code::NotAllocated => "找不到可用的員工：此員工未配置、尚未就緒、已停用或已退役，未變更。",
        Code::NotReady => "目前有工作正在執行或此員工正在使用中，未變更；請稍後再試。",
        Code::InvalidPayload => "隔離服務拒絕此資料（大小或編碼不正確），未變更。",
        Code::HomeEntryUnsafe => "員工家目錄中的 .duduclaw-api 或 providers.json 不安全（符號連結、非目錄、擁有者或權限不符），未寫入任何資料。",
        Code::StorageUnavailable => "員工儲存空間無法使用或已滿，未完成變更；請確認空間後再試。",
        Code::InvalidRequest => "隔離服務拒絕此請求，未變更。",
        // Also root's answer when its 4 s bound expired while the write may
        // still complete: unconfirmed, never "nothing changed".
        Code::IsolationUnavailable => UNCONFIRMED,
    }
}

/// The schema-5 exchange as host root. The document is moved into the
/// request (no copy) and wiped when the transport drops it.
#[cfg(target_os = "linux")]
fn exchange(target: &Target, document: Option<Zeroizing<Vec<u8>>>)
    -> Result<std::result::Result<duduclaw_migration::allocation::providers::Provisioned, duduclaw_migration::allocation::providers::ProviderErrorCode>> {
    use duduclaw_migration::allocation::{providers::{Payload, ProviderAction, ProviderRequest, SCHEMA_V5}, transport};
    let clear = document.is_none();
    let payload = match document {
        Some(bytes) => Some(Payload::new(bytes).map_err(|_| refused(INVALID))?),
        None => None,
    };
    let (employee_id, label) = match target {
        Target::Id(id) => (Some(id.clone()), None),
        Target::Label(label) => (None, Some(label.clone())),
    };
    let request = ProviderRequest { schema: SCHEMA_V5, nonce: duduclaw_isolation::ids::RequestId(*uuid::Uuid::new_v4().as_bytes()),
        action: ProviderAction::ProvisionProviders, item: None, employee_id, label, payload, clear };
    transport::provide(request).map_err(|failure| refused(transport_line(matches!(failure, transport::ProvideFailure::Unconfirmed(_)))))
}

pub(crate) fn handle(command: ProviderCommand) -> Result<()> {
    // Refuse employee sessions before anything else (no stdin read, no socket).
    operator_gate()?;
    let ProviderCommand::Set { protected_linux: _, employee_id, employee, clear } = command;
    let target = target(employee_id.as_deref(), employee.as_deref())?;
    #[cfg(target_os = "linux")]
    {
        authorize(unsafe { libc::geteuid() }, unsafe { libc::getegid() })?;
        let document = if clear {
            None
        } else {
            use std::io::IsTerminal;
            let stdin = std::io::stdin();
            let terminal = stdin.is_terminal();
            Some(read_document(&mut stdin.lock(), terminal)?)
        };
        let summary = match document.as_deref() { Some(bytes) => Some(validate(bytes)?), None => None };
        match exchange(&target, document)? {
            Ok(_) => { println!("{}", success_line(&target, summary.as_ref())); Ok(()) },
            Err(code) => Err(refused(refusal_line(code))),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (target, clear);
        Err(refused("protected-linux provider requires native Linux"))
    }
}

#[cfg(test)]
#[path = "protected_linux_provider_tests.rs"]
mod tests;
