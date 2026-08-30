//! `duduclaw compat windows-vm` — CP-2/B3: self-packaged Windows VM +
//! RemoteApp bootstrap CLI (design §2.3 路 B). See
//! `commercial/docs/DESIGN-app-compat-layer-2026-08.md` §2.3/§5 CP-2,
//! `commercial/docs/TODO-compat-cp2-2026-08.md`'s B3 row, and
//! `docs/guides/app-compat.md`.
//!
//! Wraps `dockur/windows` (a Docker-Compose-driven Windows-in-a-container
//! VM, <https://github.com/dockur/windows>, image `ghcr.io/dockur/windows`)
//! + FreeRDP 3 RemoteApp (`/app:` RAIL mode) so a single Windows
//! application appears as a native seamless window on DuDuClaw OS — the
//! same architecture WinApps/WinBoat use, self-packaged rather than
//! vendored (design §2.3: "自家包裝、不綁它們的發布節奏").
//!
//! Nothing here is pre-installed or auto-downloaded: `setup` only writes a
//! compose file and starts the container. The Windows installation image
//! itself is fetched by the `dockur/windows` container on its own first
//! boot — upstream's own "not bundled, user-triggered download" mechanism
//! is exactly design §2.3's "一鍵引導、不預裝" requirement; this module
//! doesn't ship or cache any Windows media itself.
//!
//! # Binary name: `xfreerdp3`, not `xfreerdp`
//!
//! `meta-openembedded`'s `freerdp3_3.24.2.bb` (vendored in this repo)
//! builds with `-DWITH_BINARY_VERSIONING=ON`. One-hand-verified against
//! FreeRDP's own `cmake/AddTargetWithResourceFile.cmake` macro (2026-08-30,
//! via the GitHub code-search API): when `WITH_BINARY_VERSIONING` is on,
//! an executable's `OUTPUT_NAME` becomes `"${MODULE_NAME}${major_version}"`
//! — `client/X11/CMakeLists.txt` sets `MODULE_NAME` to `"xfreerdp"`, so the
//! installed binary this image actually ships is **`xfreerdp3`**. Debian
//! testing's independent `freerdp3-x11` package corroborates this (its own
//! manpage is titled `xfreerdp3(1)`). [`resolve_xfreerdp_binary`] tries the
//! versioned name first and falls back to the unversioned one, and the
//! `windows-vm.toml` declaration's `require_tool` lists `xfreerdp3` — see
//! that file's own comment for the same note.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use console::style;
use duduclaw_core::error::{DuDuClawError, Result};
use tokio::io::AsyncWriteExt as _;

// ── Resource thresholds (design §2.3, one-hand-verified 2026-08-30 against
// the dockur/windows README + docs/environment.md) ─────────────────────

/// Host-machine total RAM below which `setup` prints a (non-blocking)
/// suggestion — design §2.3: "mini-PC 16GB RAM 建議升為 VM 路線啟用門檻".
/// Advisory only: nothing here refuses to continue past this check, matching
/// the design's own wording ("建議門檻", not a hard requirement).
pub const HOST_RECOMMENDED_RAM_GB: u64 = 16;

/// VM RAM floor (design §2.3: "VM 常駐 RAM 4GB 起"). Below this, Windows
/// itself is unlikely to boot usably, so `--ram` under this value is a hard
/// CLI error, not a warning.
pub const VM_RAM_FLOOR_GB: u64 = 4;

/// The design's "實用" (practically usable) RAM figure and this command's
/// own `--ram` default when the operator doesn't pass one.
pub const VM_RAM_DEFAULT_GB: u64 = 8;

/// VM disk floor (design §2.3: "磁碟 32GB 起").
pub const VM_DISK_FLOOR_GB: u64 = 32;

/// `--disk` default — matches `dockur/windows`'s own upstream
/// `DISK_SIZE` default (`64G`, one-hand-verified against
/// `docs/environment.md`, 2026-08-30), not a DuDuClaw-invented number.
pub const VM_DISK_DEFAULT_GB: u64 = 64;

/// `dockur/windows`'s own `CPU_CORES` default (one-hand-verified,
/// 2026-08-30). Not exposed as a `setup` flag in this wave — the TODO's B3
/// row only asks for `--ram`/`--disk`/`--version`.
pub const VM_CPU_CORES_DEFAULT: u64 = 2;

/// `dockur/windows`'s own default RDP/Windows account (one-hand-verified
/// against `docs/environment.md`: `USERNAME` defaults to `Docker`,
/// `PASSWORD` to `admin`). Not overridden by this wave's compose
/// generation — a future wave can expose `--username`/`--password` flags
/// wired to the same-named compose environment variables if needed.
pub const VM_DEFAULT_USERNAME: &str = "Docker";
pub const VM_DEFAULT_PASSWORD: &str = "admin";

/// Non-blocking advisory for the *host machine's* total RAM against the
/// design's suggested 16GB threshold (§2.3). `None` when the host meets the
/// threshold, or when RAM couldn't be determined at all — a missing
/// `/proc/meminfo` must not block `setup`, only skip the advisory
/// (fail-open, matching this crate's "advisory checks degrade to silence,
/// not to a refusal" convention).
pub fn host_ram_advisory(total_host_ram_gb: Option<u64>) -> Option<String> {
    match total_host_ram_gb {
        Some(gb) if gb < HOST_RECOMMENDED_RAM_GB => Some(format!(
            "本機總記憶體約 {gb}GB，低於設計文件建議的 {HOST_RECOMMENDED_RAM_GB}GB 門檻（值班機硬體規格建議 \
             16GB 以上再啟用 VM 路）。VM 本身常駐 RAM {VM_RAM_FLOOR_GB}GB 起、實用建議 {VM_RAM_DEFAULT_GB}GB，\
             資源可能吃緊——這只是建議，不會擋下設定，是否繼續請自行評估。"
        )),
        _ => None,
    }
}

/// Pure parser for `/proc/meminfo`'s `MemTotal:` line (kB), isolated from
/// filesystem I/O so it's directly unit-testable against fixture text.
pub fn parse_meminfo_total_kb(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        rest.trim().split_whitespace().next()?.parse::<u64>().ok()
    })
}

/// Read a `/proc/meminfo`-shaped file's `MemTotal` into whole GB (floor).
/// Path-injected (not hardcoded to `/proc/meminfo`) so tests can point it
/// at a fixture — the same "env/path override for testability" convention
/// `compat_runners::COMPAT_DIRS_ENV` and this module's
/// [`DUDUCLAW_COMPAT_KVM_DEVICE_ENV`] use.
pub fn read_host_ram_gb(proc_meminfo_path: &Path) -> Option<u64> {
    let contents = std::fs::read_to_string(proc_meminfo_path).ok()?;
    parse_meminfo_total_kb(&contents).map(|kb| kb / (1024 * 1024))
}

/// Hard floor for the VM's own `--ram`. Unlike [`host_ram_advisory`] this
/// rejects — a VM given less than dockur's documented practical minimum
/// won't boot Windows usably, so it's not worth generating a compose file
/// for.
pub fn validate_vm_ram_gb(ram_gb: u64) -> std::result::Result<(), String> {
    if ram_gb < VM_RAM_FLOOR_GB {
        Err(format!(
            "--ram {ram_gb} 低於下限 {VM_RAM_FLOOR_GB}GB（設計文件：VM 常駐 RAM {VM_RAM_FLOOR_GB}GB 起）；\
             請至少指定 {VM_RAM_FLOOR_GB}，建議 {VM_RAM_DEFAULT_GB} 以上。"
        ))
    } else {
        Ok(())
    }
}

/// Hard floor for the VM's own `--disk` (design §2.3: "磁碟 32GB 起").
pub fn validate_vm_disk_gb(disk_gb: u64) -> std::result::Result<(), String> {
    if disk_gb < VM_DISK_FLOOR_GB {
        Err(format!("--disk {disk_gb} 低於下限 {VM_DISK_FLOOR_GB}GB（設計文件：磁碟 {VM_DISK_FLOOR_GB}GB 起）。"))
    } else {
        Ok(())
    }
}

// ── KVM fail-closed gate ────────────────────────────────────────────────

/// Test-only override for the KVM device path this command checks — same
/// "env override so tests don't touch the real filesystem" convention as
/// `compat_runners::COMPAT_DIRS_ENV`.
pub const DUDUCLAW_COMPAT_KVM_DEVICE_ENV: &str = "DUDUCLAW_COMPAT_KVM_DEVICE";

/// Resolve the KVM device path this process actually checks — the real
/// `/dev/kvm` unless overridden for tests.
pub fn kvm_device_path() -> PathBuf {
    std::env::var_os(DUDUCLAW_COMPAT_KVM_DEVICE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/dev/kvm"))
}

/// Fail-closed hardware-virtualization gate (design §2.3 / TODO B3: "KVM
/// fail-closed"). `dockur/windows` hard-requires `/dev/kvm` — one-hand-
/// verified against upstream's Requirements section, 2026-08-30: no
/// software/TCG fallback is documented. A QEMU test environment with no
/// nested KVM is exactly the "not available" case this must refuse, not
/// silently degrade into an unusably slow software-emulated VM.
pub fn check_kvm_device(path: &Path) -> std::result::Result<(), String> {
    if path.exists() {
        Ok(())
    } else {
        Err(format!(
            "找不到硬體虛擬化裝置 {}——本機可能未啟用 CPU 虛擬化功能（BIOS/UEFI VT-x／AMD-V）、\
             未載入 kvm 核心模組，或執行環境本身不支援巢狀虛擬化（例如在 QEMU 測試映像裡執行）。\
             dockur/windows 硬性要求 KVM，沒有軟體模擬（TCG）備援——沒有這個裝置，Windows VM 不會被啟動。",
            path.display()
        ))
    }
}

// ── License-responsibility disclosure (design §2.3, verbatim) ──────────

/// Confirmation phrase the operator must type verbatim (non-`--yes` path)
/// to acknowledge the disclosure below — the codebase's existing "type an
/// exact phrase to confirm a consequential action" shape (CLAUDE.md §6's
/// gate spirit: a human explicitly passes this gate, not just presses
/// Enter).
pub const LICENSE_CONFIRM_PHRASE: &str = "I-HAVE-A-LICENSE";

/// The three-element license-responsibility disclosure design §2.3
/// mandates be shown verbatim before `setup` proceeds:
/// 1. 需自備 Windows 11 Pro 以上授權
/// 2. Home 版不支援 RemoteApp 是 WinApps 明文硬要求
/// 3. OEM 授權通常不含虛擬化權利是 Microsoft 原文
///
/// Kept as one constant (not assembled ad hoc per print call) so a unit
/// test can assert none of the three legally-load-bearing claims silently
/// drift.
pub const LICENSE_DISCLOSURE_ZH_TW: &str = "\
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
授權責任揭露 — 請詳閱後再繼續
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
這個功能會在本機建立一台完整的 Windows 虛擬機，讓你透過無縫視窗執行 Windows 應用程式
（例如 Microsoft 365、AutoCAD、記帳／ERP 軟體）。DuDuClaw 不隨機出貨、不代購、不代管
任何 Windows 授權金鑰——在你繼續之前，請確認以下三點：

  1. 你需要自備 Windows 11 Pro 以上版本的合法授權。這台虛擬機安裝的是 Windows，其授權
     責任由你自行承擔，DuDuClaw 不提供、也不附贈授權。

  2. Home 版不支援 RemoteApp 無縫視窗——這是上游 WinApps 專案文件明文列出的硬性需求，
     不是 DuDuClaw 的限制。用 Home 版授權裝這台 VM，應用程式仍可能裝得起來，但「無縫
     視窗」這個體驗本身跑不出來。

  3. 你電腦／筆電隨附的 OEM 授權，通常不含虛擬化使用權利——這是 Microsoft 官方授權
     條款的原文立場，不是 DuDuClaw 的解讀。用機器原廠內建的 Windows 授權裝進這台虛擬
     機，可能違反授權條款。

Windows 安裝映像不會預先下載或內建——下一步啟動容器後，安裝映像由容器在你的觸發下
自行下載（不是 DuDuClaw 提供的映像檔）。
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
";

// ── compose.yaml generation ─────────────────────────────────────────────

/// Parameters for [`render_windows_vm_compose`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsVmComposeParams {
    /// `dockur/windows`'s `VERSION` value (e.g. `"11"` for Windows 11 Pro
    /// — the upstream default; one-hand-verified against the project's own
    /// version table, 2026-08-30).
    pub version: String,
    pub ram_gb: u64,
    pub disk_gb: u64,
    pub cpu_cores: u64,
    /// Absolute host path the `/storage` volume binds to — the "資源檔落
    /// 資料分割區" requirement (design §2.3), i.e. always somewhere under
    /// `duduclaw_home()`, never the read-only rootfs. Not user-controlled
    /// free text (always derived from `duduclaw_home()` by the caller), so
    /// this renderer does not attempt YAML-string escaping beyond the
    /// surrounding double quotes.
    pub storage_dir: String,
}

/// Render the `compose.yaml` handed to `docker compose up`.
///
/// Shape one-hand-verified against the `dockur/windows`
/// (`ghcr.io/dockur/windows`) README + `docs/environment.md`
/// (2026-08-30): required `devices` (`/dev/kvm`, `/dev/net/tun`),
/// `cap_add: NET_ADMIN`, the `/storage` volume, and the
/// `VERSION`/`RAM_SIZE`/`CPU_CORES`/`DISK_SIZE` environment variables
/// (`RAM_SIZE`/`DISK_SIZE` take a `<N>G` string, not a bare number).
///
/// Two deliberate departures from the upstream example compose file, both
/// security-motivated (CLAUDE.md's "deny by default" convention — matching
/// this codebase's other loopback-only surfaces, e.g. `mcp_http_server`'s
/// `127.0.0.1:8765`, `runtime_status`'s loopback-only endpoint):
///
/// - Ports are bound to `127.0.0.1` only, never every interface — the
///   upstream example exposes the web viewer (8006) and RDP (3389) on
///   `0.0.0.0`; this VM's management surface has no business being
///   reachable off-box.
/// - `restart: unless-stopped` instead of upstream's `restart: always` —
///   `always` resurrects the container even after an operator-intended
///   `docker compose down` on the next Docker daemon start; `unless-
///   stopped` is compose's own "stay down if a human stopped you"
///   semantics.
pub fn render_windows_vm_compose(params: &WindowsVmComposeParams) -> String {
    format!(
        r#"services:
  windows:
    image: ghcr.io/dockur/windows
    container_name: duduclaw-windows-vm
    environment:
      VERSION: "{version}"
      RAM_SIZE: "{ram_gb}G"
      CPU_CORES: "{cpu_cores}"
      DISK_SIZE: "{disk_gb}G"
    devices:
      - /dev/kvm
      - /dev/net/tun
    cap_add:
      - NET_ADMIN
    ports:
      - "127.0.0.1:8006:8006"
      - "127.0.0.1:3389:3389/tcp"
      - "127.0.0.1:3389:3389/udp"
    volumes:
      - "{storage_dir}:/storage"
    restart: unless-stopped
    stop_grace_period: 2m
"#,
        version = params.version,
        ram_gb = params.ram_gb,
        cpu_cores = params.cpu_cores,
        disk_gb = params.disk_gb,
        storage_dir = params.storage_dir,
    )
}

/// Atomic commit (sibling temp file + rename, matching
/// `duduclaw_core::org_store`/`preset`'s own `write_atomic` shape) with a
/// `0600` mode — the compose file's env block doesn't currently carry a
/// secret (no `USERNAME`/`PASSWORD` override in this wave), but it
/// configures a locally-reachable RDP endpoint, so it's treated as
/// sensitive by default rather than only after a future wave adds
/// credential overrides.
fn write_compose_file_0600(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn windows_vm_dir(home: &Path) -> PathBuf {
    home.join("windows-vm")
}

fn compose_path(home: &Path) -> PathBuf {
    windows_vm_dir(home).join("compose.yaml")
}

// ── RemoteApp launch (xfreerdp3) ────────────────────────────────────────

/// Resolve which RemoteApp client binary is actually on `$PATH` — see this
/// module's top doc comment for why the versioned name is tried first.
pub fn resolve_xfreerdp_binary() -> &'static str {
    resolve_xfreerdp_binary_with(duduclaw_core::compat_runners::tool_on_path)
}

/// [`resolve_xfreerdp_binary`] with the PATH-resolution predicate
/// injected, so the fallback branch is directly unit-testable without
/// depending on whether `xfreerdp3` happens to be installed on the machine
/// running the test suite (the same "inject the checked thing" shape as
/// [`check_kvm_device`]'s path parameter).
fn resolve_xfreerdp_binary_with(tool_on_path: impl Fn(&str) -> bool) -> &'static str {
    if tool_on_path("xfreerdp3") {
        "xfreerdp3"
    } else {
        "xfreerdp"
    }
}

/// Build the RemoteApp/RAIL argv for launching one Windows executable as a
/// seamless window (design §2.3 路 B, TODO B3's `app` row). Syntax
/// one-hand-verified against the FreeRDP 3 CLI reference (Debian testing's
/// `xfreerdp3(1)` manpage, 2026-08-30).
///
/// `/p:<password>` is deliberately never used — a password on argv is
/// readable by any local user via `/proc/<pid>/cmdline` or `ps aux`.
/// `/from-stdin:force` makes FreeRDP read the password from its own stdin
/// instead; the caller ([`crate::cmd_compat_windows_vm_app`] — see that
/// function) feeds it programmatically through the spawned child's stdin
/// pipe, never as a CLI argument.
pub fn build_xfreerdp_remoteapp_args(
    exe: &str,
    display_name: Option<&str>,
    rdp_host_port: &str,
    username: &str,
) -> Vec<String> {
    let mut app_value = format!("program:{exe}");
    if let Some(name) = display_name {
        // FreeRDP's /app: value is itself comma-separated
        // (`program:...,cmd:...,name:...`) — a comma inside the display
        // name would be misread as a new sub-field, so it's replaced
        // rather than trusted verbatim (CLAUDE.md "validate at system
        // boundaries").
        let safe_name = name.replace(',', " ");
        app_value.push_str(&format!(",name:{safe_name}"));
    }
    vec![
        format!("/v:{rdp_host_port}"),
        format!("/u:{username}"),
        "/from-stdin:force".to_string(),
        "/cert:ignore".to_string(),
        format!("/app:{app_value}"),
    ]
}

// ── CLI commands ─────────────────────────────────────────────────────────

/// `duduclaw compat windows-vm setup [--yes] [--ram <GB>] [--disk <GB>]
/// [--version 11]`.
///
/// Flow (design §2.3 / TODO B3, in order — see this function's inline
/// section markers): ① host RAM advisory (never blocks) → ② KVM
/// fail-closed (hard refuse) → ③ license disclosure + confirmation → ④
/// compose.yaml generation → ⑤ `docker compose up -d` → ⑥ status +
/// next-steps summary.
pub async fn cmd_compat_windows_vm_setup(
    auto_yes: bool,
    ram_gb: Option<u64>,
    disk_gb: Option<u64>,
    version: String,
) -> Result<()> {
    let ram_gb = ram_gb.unwrap_or(VM_RAM_DEFAULT_GB);
    let disk_gb = disk_gb.unwrap_or(VM_DISK_DEFAULT_GB);
    validate_vm_ram_gb(ram_gb).map_err(DuDuClawError::Config)?;
    validate_vm_disk_gb(disk_gb).map_err(DuDuClawError::Config)?;

    // ① Host resource advisory — non-blocking (design §2.3: "建議門檻").
    let host_ram_gb = read_host_ram_gb(Path::new("/proc/meminfo"));
    if let Some(msg) = host_ram_advisory(host_ram_gb) {
        println!("{}", style(format!("⚠ {msg}")).yellow());
        println!();
    }

    // ② KVM fail-closed — never continues past a missing device.
    if let Err(msg) = check_kvm_device(&kvm_device_path()) {
        return Err(DuDuClawError::Config(msg));
    }
    println!("{}", style("✓ 硬體虛擬化裝置（/dev/kvm）可用").green());
    println!();

    // ③ License-responsibility disclosure — always shown; gated on an
    // explicit confirmation unless --yes.
    println!("{LICENSE_DISCLOSURE_ZH_TW}");
    if !auto_yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(DuDuClawError::Config(format!(
                "非互動模式且未帶 --yes：請詳閱以上授權責任揭露後，重新執行並加上 --yes \
                 （代表你已閱讀並同意自行承擔授權責任），或改在互動終端機手動輸入確認字樣「{LICENSE_CONFIRM_PHRASE}」。"
            )));
        }
        print!("請輸入「{LICENSE_CONFIRM_PHRASE}」以確認你已詳閱並同意自行承擔上述授權責任：");
        std::io::stdout().flush().ok();
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|e| DuDuClawError::Config(format!("讀取輸入失敗：{e}")))?;
        if answer.trim() != LICENSE_CONFIRM_PHRASE {
            return Err(DuDuClawError::Config("未確認授權責任揭露，已取消設定。".to_string()));
        }
    }
    println!("{}", style("✓ 授權責任揭露已確認").green());
    println!();

    // ④ compose.yaml generation — resource files land on the data
    // partition (`duduclaw_home()`), never the read-only rootfs.
    let home = crate::duduclaw_home();
    let vm_dir = windows_vm_dir(&home);
    let storage_dir = vm_dir.join("storage");
    std::fs::create_dir_all(&storage_dir)
        .map_err(|e| DuDuClawError::Config(format!("建立資料目錄失敗（{}）：{e}", storage_dir.display())))?;
    let compose_file = compose_path(&home);
    let params = WindowsVmComposeParams {
        version: version.clone(),
        ram_gb,
        disk_gb,
        cpu_cores: VM_CPU_CORES_DEFAULT,
        storage_dir: storage_dir.to_string_lossy().to_string(),
    };
    let compose_yaml = render_windows_vm_compose(&params);
    write_compose_file_0600(&compose_file, &compose_yaml)
        .map_err(|e| DuDuClawError::Config(format!("寫入 compose 設定失敗（{}）：{e}", compose_file.display())))?;
    println!("✓ 已產生 compose 設定：{}", compose_file.display());
    println!();

    // ⑤ Start the container. Errors are relayed verbatim (design/TODO's
    // "錯誤誠實轉述") rather than paraphrased.
    println!("啟動容器中……（首次啟動會由容器自行下載 Windows 安裝映像，依網路與版本可能需要相當長時間）");
    let output = tokio::process::Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(&compose_file)
        .arg("up")
        .arg("-d")
        .output()
        .await
        .map_err(|e| {
            DuDuClawError::Container(format!("執行 `docker compose up -d` 失敗：{e}（docker 是否已安裝？見 `duduclaw compat list`）"))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DuDuClawError::Container(format!(
            "`docker compose up -d` 結束碼非零（{}）：\n{}",
            output.status,
            stderr.trim()
        )));
    }

    // ⑥ Status + next steps.
    println!("{}", style("✓ Windows VM 容器已啟動").green());
    println!();
    println!("下一步：");
    println!(
        "  • 用瀏覽器打開 http://127.0.0.1:8006 監看安裝進度（首次啟動會自動下載並安裝 \
         Windows（VERSION={version}）——這是容器自己觸發的下載，不是 DuDuClaw 內建的映像）。"
    );
    println!("  • `duduclaw compat windows-vm status` 查看容器狀態。");
    println!("  • 安裝完成後，`duduclaw compat windows-vm app <程式路徑>` 以無縫視窗啟動一個 Windows 應用程式。");
    println!(
        "  • 初始帳密：{VM_DEFAULT_USERNAME} / {VM_DEFAULT_PASSWORD}（dockur/windows 預設值，建議進 Windows 後自行更改）。"
    );
    Ok(())
}

/// `duduclaw compat windows-vm status`.
pub async fn cmd_compat_windows_vm_status() -> Result<()> {
    let home = crate::duduclaw_home();
    let compose_file = compose_path(&home);
    if !compose_file.is_file() {
        println!("尚未設定 Windows VM——執行 `duduclaw compat windows-vm setup` 開始。");
        return Ok(());
    }

    let output = tokio::process::Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(&compose_file)
        .arg("ps")
        .output()
        .await
        .map_err(|e| DuDuClawError::Container(format!("執行 `docker compose ps` 失敗：{e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DuDuClawError::Container(format!(
            "`docker compose ps` 結束碼非零（{}）：\n{}",
            output.status,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        println!(
            "compose 設定存在（{}），但目前沒有執行中的容器。\n\
             執行 `duduclaw compat windows-vm setup` 重新啟動，或手動執行：\n  \
             docker compose -f {} up -d",
            compose_file.display(),
            compose_file.display()
        );
    } else {
        println!("{}", stdout.trim_end());
        println!();
        println!("管理入口：http://127.0.0.1:8006（安裝／監看畫面）　RDP：127.0.0.1:3389");
    }
    Ok(())
}

/// `duduclaw compat windows-vm app <exe> [--name <顯示名>]`.
///
/// Requires an X11-capable display — RAIL/RemoteApp is an X11-client
/// feature of FreeRDP 3 (`xfreerdp3` links against `libX11` in this
/// image's `PACKAGECONFIG[x11]` build), so it must run inside the kiosk
/// session's XWayland (see `docs/guides/app-compat.md`'s VM section and
/// CP-1's own XWayland landing note), the same prerequisite Bottles/Wine
/// already depends on.
pub async fn cmd_compat_windows_vm_app(exe: String, display_name: Option<String>) -> Result<()> {
    let home = crate::duduclaw_home();
    let compose_file = compose_path(&home);
    if !compose_file.is_file() {
        return Err(DuDuClawError::Config("尚未設定 Windows VM——先執行 `duduclaw compat windows-vm setup`。".to_string()));
    }

    let binary = resolve_xfreerdp_binary();
    let args = build_xfreerdp_remoteapp_args(&exe, display_name.as_deref(), "127.0.0.1:3389", VM_DEFAULT_USERNAME);

    println!("以無縫視窗啟動：{exe}（透過 {binary}——需在 X11/XWayland 環境下執行，見 docs/guides/app-compat.md）");

    let mut child = tokio::process::Command::new(binary)
        .args(&args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| DuDuClawError::Container(format!("啟動 {binary} 失敗：{e}（是否已安裝？見 `duduclaw compat list`）")))?;

    // `/from-stdin:force` reads the RDP password from our stdin pipe —
    // never argv, so it never appears in `ps`/`/proc/<pid>/cmdline`.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{VM_DEFAULT_PASSWORD}\n").as_bytes()).await;
    }

    let status =
        child.wait().await.map_err(|e| DuDuClawError::Container(format!("{binary} 執行錯誤：{e}")))?;
    if !status.success() {
        return Err(DuDuClawError::Container(format!(
            "{binary} 結束（{status}）——RDP 連線可能失敗，或 Windows VM 尚未完成安裝／尚未啟動。"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // ── RAM threshold judgement ──────────────────────────────────────

    #[test]
    fn host_ram_advisory_fires_below_threshold() {
        let msg = host_ram_advisory(Some(8)).expect("8GB must trigger the advisory");
        assert!(msg.contains("8GB"));
        assert!(msg.contains(&HOST_RECOMMENDED_RAM_GB.to_string()));
    }

    #[test]
    fn host_ram_advisory_silent_at_or_above_threshold() {
        assert!(host_ram_advisory(Some(HOST_RECOMMENDED_RAM_GB)).is_none());
        assert!(host_ram_advisory(Some(32)).is_none());
    }

    #[test]
    fn host_ram_advisory_silent_when_undetermined() {
        // Fail-open: an unreadable /proc/meminfo must not block setup, so
        // the advisory itself stays silent rather than guessing.
        assert!(host_ram_advisory(None).is_none());
    }

    #[test]
    fn parse_meminfo_total_kb_reads_the_memtotal_line() {
        let fixture = "MemTotal:       16384000 kB\nMemFree:         1000000 kB\n";
        assert_eq!(parse_meminfo_total_kb(fixture), Some(16384000));
    }

    #[test]
    fn parse_meminfo_total_kb_missing_line_is_none() {
        assert_eq!(parse_meminfo_total_kb("MemFree: 1000 kB\n"), None);
        assert_eq!(parse_meminfo_total_kb(""), None);
    }

    #[test]
    fn read_host_ram_gb_end_to_end_via_fixture_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meminfo");
        std::fs::write(&path, "MemTotal:       16777216 kB\n").unwrap();
        assert_eq!(read_host_ram_gb(&path), Some(16));
    }

    #[test]
    fn read_host_ram_gb_missing_file_is_none() {
        assert_eq!(read_host_ram_gb(Path::new("/definitely/does/not/exist/meminfo")), None);
    }

    #[test]
    fn validate_vm_ram_gb_rejects_below_floor() {
        assert!(validate_vm_ram_gb(VM_RAM_FLOOR_GB - 1).is_err());
        assert!(validate_vm_ram_gb(VM_RAM_FLOOR_GB).is_ok());
        assert!(validate_vm_ram_gb(VM_RAM_DEFAULT_GB).is_ok());
    }

    #[test]
    fn validate_vm_disk_gb_rejects_below_floor() {
        assert!(validate_vm_disk_gb(VM_DISK_FLOOR_GB - 1).is_err());
        assert!(validate_vm_disk_gb(VM_DISK_FLOOR_GB).is_ok());
        assert!(validate_vm_disk_gb(VM_DISK_DEFAULT_GB).is_ok());
    }

    // ── KVM fail-closed, path-injected ───────────────────────────────

    #[test]
    fn check_kvm_device_ok_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let fake_kvm = dir.path().join("kvm");
        std::fs::write(&fake_kvm, b"").unwrap();
        assert!(check_kvm_device(&fake_kvm).is_ok());
    }

    #[test]
    fn check_kvm_device_fails_closed_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("kvm-does-not-exist");
        let err = check_kvm_device(&missing).expect_err("missing /dev/kvm must be a hard error");
        assert!(err.contains("虛擬化"));
        assert!(err.contains(&missing.display().to_string()));
    }

    #[test]
    fn kvm_device_path_honours_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os(DUDUCLAW_COMPAT_KVM_DEVICE_ENV);
        unsafe { std::env::set_var(DUDUCLAW_COMPAT_KVM_DEVICE_ENV, "/tmp/not-a-real-kvm-device") };
        assert_eq!(kvm_device_path(), PathBuf::from("/tmp/not-a-real-kvm-device"));
        unsafe {
            match &prev {
                Some(v) => std::env::set_var(DUDUCLAW_COMPAT_KVM_DEVICE_ENV, v),
                None => std::env::remove_var(DUDUCLAW_COMPAT_KVM_DEVICE_ENV),
            }
        }
    }

    // ── Disclosure text: the three mandated elements must be present ──

    #[test]
    fn license_disclosure_carries_all_three_mandated_elements() {
        let text = LICENSE_DISCLOSURE_ZH_TW;
        // 1. must自備 Windows 11 Pro 以上授權
        assert!(text.contains("Windows 11 Pro"), "element 1 (Windows 11 Pro+) missing");
        assert!(text.contains("自備"), "element 1 (自備) missing");
        // 2. Home 版不支援 RemoteApp 是 WinApps 明文硬要求
        assert!(text.contains("Home"), "element 2 (Home) missing");
        assert!(text.contains("RemoteApp"), "element 2 (RemoteApp) missing");
        assert!(text.contains("WinApps"), "element 2 (WinApps attribution) missing");
        // 3. OEM 授權通常不含虛擬化權利是 Microsoft 原文
        assert!(text.contains("OEM"), "element 3 (OEM) missing");
        assert!(text.contains("虛擬化"), "element 3 (虛擬化權利) missing");
        assert!(text.contains("Microsoft"), "element 3 (Microsoft attribution) missing");
    }

    #[test]
    fn license_confirm_phrase_is_stable() {
        // A regression test as much as a behavior test — this phrase is
        // what an operator types at a live terminal; changing it silently
        // would be a UX break, not just an internal refactor.
        assert_eq!(LICENSE_CONFIRM_PHRASE, "I-HAVE-A-LICENSE");
    }

    // ── compose.yaml generation: golden comparison ───────────────────

    fn sample_params() -> WindowsVmComposeParams {
        WindowsVmComposeParams {
            version: "11".to_string(),
            ram_gb: 8,
            disk_gb: 64,
            cpu_cores: 2,
            storage_dir: "/home/duduclaw/.duduclaw/windows-vm/storage".to_string(),
        }
    }

    #[test]
    fn render_windows_vm_compose_golden() {
        let expected = r#"services:
  windows:
    image: ghcr.io/dockur/windows
    container_name: duduclaw-windows-vm
    environment:
      VERSION: "11"
      RAM_SIZE: "8G"
      CPU_CORES: "2"
      DISK_SIZE: "64G"
    devices:
      - /dev/kvm
      - /dev/net/tun
    cap_add:
      - NET_ADMIN
    ports:
      - "127.0.0.1:8006:8006"
      - "127.0.0.1:3389:3389/tcp"
      - "127.0.0.1:3389:3389/udp"
    volumes:
      - "/home/duduclaw/.duduclaw/windows-vm/storage:/storage"
    restart: unless-stopped
    stop_grace_period: 2m
"#;
        assert_eq!(render_windows_vm_compose(&sample_params()), expected);
    }

    #[test]
    fn render_windows_vm_compose_contains_required_env_devices_and_volume() {
        let yaml = render_windows_vm_compose(&sample_params());
        for needle in [
            "image: ghcr.io/dockur/windows",
            "VERSION: \"11\"",
            "RAM_SIZE: \"8G\"",
            "CPU_CORES: \"2\"",
            "DISK_SIZE: \"64G\"",
            "- /dev/kvm",
            "- /dev/net/tun",
            "- NET_ADMIN",
            "/storage",
            "127.0.0.1:8006:8006",
            "127.0.0.1:3389:3389/tcp",
        ] {
            assert!(yaml.contains(needle), "compose YAML missing expected fragment: {needle:?}\n{yaml}");
        }
        // Never bind to every interface — this is the whole point of the
        // loopback-only departure from upstream's example.
        assert!(!yaml.contains("0.0.0.0"));
    }

    #[test]
    fn render_windows_vm_compose_reflects_custom_ram_disk_version() {
        let params = WindowsVmComposeParams {
            version: "11l".to_string(),
            ram_gb: 16,
            disk_gb: 128,
            cpu_cores: 4,
            storage_dir: "/data/windows-vm/storage".to_string(),
        };
        let yaml = render_windows_vm_compose(&params);
        assert!(yaml.contains("VERSION: \"11l\""));
        assert!(yaml.contains("RAM_SIZE: \"16G\""));
        assert!(yaml.contains("DISK_SIZE: \"128G\""));
        assert!(yaml.contains("CPU_CORES: \"4\""));
        assert!(yaml.contains("/data/windows-vm/storage:/storage"));
    }

    // ── xfreerdp RemoteApp argv building ─────────────────────────────

    #[test]
    fn build_xfreerdp_remoteapp_args_never_puts_password_on_argv() {
        let args = build_xfreerdp_remoteapp_args("winword.exe", None, "127.0.0.1:3389", "Docker");
        for arg in &args {
            assert!(!arg.contains("admin"), "password leaked into argv: {arg:?}");
            assert!(!arg.starts_with("/p:"), "must never use /p: (argv-visible password)");
        }
        assert!(args.contains(&"/from-stdin:force".to_string()));
        assert!(args.iter().any(|a| a == "/v:127.0.0.1:3389"));
        assert!(args.iter().any(|a| a == "/u:Docker"));
        assert!(args.iter().any(|a| a == "/app:program:winword.exe"));
    }

    #[test]
    fn build_xfreerdp_remoteapp_args_with_display_name() {
        let args = build_xfreerdp_remoteapp_args("winword.exe", Some("Word"), "127.0.0.1:3389", "Docker");
        assert!(args.iter().any(|a| a == "/app:program:winword.exe,name:Word"));
    }

    #[test]
    fn build_xfreerdp_remoteapp_args_sanitizes_comma_in_display_name() {
        let args = build_xfreerdp_remoteapp_args("app.exe", Some("A, B"), "127.0.0.1:3389", "Docker");
        let app_flag = args.iter().find(|a| a.starts_with("/app:")).unwrap();
        // The sanitized name must not introduce a new comma-separated
        // sub-field FreeRDP would misparse.
        assert_eq!(app_flag.matches(',').count(), 1);
        assert!(app_flag.contains("name:A  B"));
    }

    // ── binary resolution ─────────────────────────────────────────────

    #[test]
    fn resolve_xfreerdp_binary_prefers_versioned_name_when_present() {
        assert_eq!(resolve_xfreerdp_binary_with(|tool| tool == "xfreerdp3"), "xfreerdp3");
    }

    #[test]
    fn resolve_xfreerdp_binary_falls_back_when_versioned_name_absent() {
        assert_eq!(resolve_xfreerdp_binary_with(|_| false), "xfreerdp");
    }
}
